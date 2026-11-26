//! Worker-only media implementation. Never linked into the GUI/backend.
#![forbid(unsafe_code)]

use image::{ImageDecoder, ImageFormat, ImageReader, imageops::FilterType};
use ncat_core::{fit_size, natural_cmp, rgba_len};
use ncat_platform::{Revision, name_bytes};
use ncat_protocol::{
    ErrorCode as Error, FrameInfo, INDEX_BYTES, MAX_ENTRIES, MAX_INPUT, MAX_OUTPUT, MAX_PIXELS,
    Page, PageKey, RawPath,
};
use std::cell::RefCell;
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;
mod animation;
mod metadata;
use animation::AnimationSession;

#[derive(Clone, Copy)]
struct RenderSpec {
    width: u32,
    height: u32,
    quarter_turns: u8,
}

trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

/// A file's stat length is only a hint: cap actual reads too, including repeated seeks.
struct LimitedReader<R> {
    inner: R,
    remaining: u64,
}
impl<R> LimitedReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            remaining: MAX_INPUT,
        }
    }
}
impl<R: Read> Read for LimitedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.remaining == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "input limit"));
        }
        let len = buf
            .len()
            .min(usize::try_from(self.remaining).unwrap_or(usize::MAX));
        let n = self.inner.read(&mut buf[..len])?;
        self.remaining -= n as u64;
        Ok(n)
    }
}
impl<R: Seek> Seek for LimitedReader<R> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let position = self.inner.seek(position)?;
        if position > MAX_INPUT {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "input limit"));
        }
        Ok(position)
    }
}

enum Source {
    Folder {
        root: PathBuf,
        initial: Option<PathBuf>,
    },
    Zip {
        path: PathBuf,
        revision: Revision,
        archive: RefCell<zip::ZipArchive<File>>,
    },
}
pub struct Book {
    animation: RefCell<Option<AnimationSession>>,
    source: Source,
    pub pages: Vec<Page>,
    pub selected: usize,
    pub title: String,
    pub deferred: bool,
}

use ncat_protocol::image_filename_hint as image_hint;

fn label(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .filter(|c| {
            !c.is_control() && !matches!(*c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .take(ncat_protocol::MAX_LABEL_CHARS)
        .collect()
}

fn checked_catalog(pages: &[Page]) -> Result<(), Error> {
    let cost: u64 = pages.iter().map(Page::cost).sum();
    if pages.len() > MAX_ENTRIES || cost > INDEX_BYTES / 2 {
        return Err(Error::Limit);
    }
    Ok(())
}

impl Book {
    pub fn open(path: &Path) -> Result<Self, Error> {
        if name_bytes(path.as_os_str()).len() > ncat_protocol::MAX_PATH_BYTES {
            return Err(Error::Limit);
        }
        let meta = fs::metadata(path).map_err(|_| Error::Io)?;
        if meta.is_dir() {
            let mut book = Self {
                animation: RefCell::new(None),
                source: Source::Folder {
                    root: path.into(),
                    initial: None,
                },
                pages: Vec::new(),
                selected: 0,
                title: label(name_bytes(path.file_name().unwrap_or(path.as_os_str()))),
                deferred: false,
            };
            book.catalog()?;
            return Ok(book);
        }
        if !meta.is_file() {
            return Err(Error::Unsupported);
        }
        let mut file = File::open(path).map_err(|_| Error::Io)?;
        let revision = Revision::of(&file)?;
        let mut header = [0; 8];
        let n = file.read(&mut header).map_err(|_| Error::Io)?;
        if n >= 4 && matches!(&header[..4], b"PK\x03\x04" | b"PK\x05\x06" | b"PK\x07\x08") {
            file.seek(SeekFrom::Start(0)).map_err(|_| Error::Io)?;
            let archive = zip::ZipArchive::new(file).map_err(|_| Error::Corrupt)?;
            let mut book = Self {
                animation: RefCell::new(None),
                source: Source::Zip {
                    path: path.into(),
                    revision,
                    archive: RefCell::new(archive),
                },
                pages: Vec::new(),
                selected: 0,
                title: label(name_bytes(path.file_name().unwrap_or(path.as_os_str()))),
                deferred: false,
            };
            book.catalog()?;
            return Ok(book);
        }
        if header.starts_with(b"Rar!") || header.starts_with(b"7z\xbc\xaf") {
            return Err(Error::Unsupported);
        }
        let root = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        Ok(Self {
            animation: RefCell::new(None),
            source: Source::Folder {
                root: root.into(),
                initial: Some(path.into()),
            },
            pages: vec![Page {
                key: PageKey::File(RawPath::new(path)),
                name: label(name_bytes(path.file_name().unwrap_or(path.as_os_str()))),
            }],
            selected: 0,
            title: label(name_bytes(root.file_name().unwrap_or(root.as_os_str()))),
            deferred: true,
        })
    }

    pub fn catalog(&mut self) -> Result<(), Error> {
        let mut pages = Vec::new();
        let mut cost = 0_u64;
        match &self.source {
            Source::Folder { root, initial } => {
                for entry in fs::read_dir(root).map_err(|_| Error::Io)? {
                    let entry = entry.map_err(|_| Error::Io)?;
                    let name = entry.file_name();
                    if !entry.file_type().map_err(|_| Error::Io)?.is_file()
                        || !image_hint(name_bytes(&name))
                    {
                        continue;
                    }
                    let page = Page {
                        key: PageKey::File(RawPath::new(&entry.path())),
                        name: label(name_bytes(&name)),
                    };
                    cost = cost.checked_add(page.cost()).ok_or(Error::Limit)?;
                    if pages.len() >= MAX_ENTRIES || cost > INDEX_BYTES / 2 {
                        return Err(Error::Limit);
                    }
                    pages.push(page);
                }
                if let Some(initial) = initial {
                    let key = PageKey::File(RawPath::new(initial));
                    if !pages.iter().any(|p| p.key == key) {
                        pages.push(Page {
                            key,
                            name: label(name_bytes(
                                initial.file_name().unwrap_or(initial.as_os_str()),
                            )),
                        });
                    }
                }
                pages.sort_by(|a, b| match (&a.key, &b.key) {
                    (PageKey::File(a), PageKey::File(b)) => natural_cmp(
                        &ncat_platform::path_sort_bytes(a),
                        &ncat_platform::path_sort_bytes(b),
                    )
                    .then_with(|| a.0.cmp(&b.0)),
                    _ => unreachable!(),
                });
                self.selected = initial
                    .as_ref()
                    .and_then(|path| {
                        pages
                            .iter()
                            .position(|p| p.key == PageKey::File(RawPath::new(path)))
                    })
                    .unwrap_or(0);
            }
            Source::Zip {
                path,
                revision,
                archive,
            } => {
                if &Revision::read(path)? != revision {
                    return Err(Error::Changed);
                }
                // Keep the already-parsed central directory and source handle for this book.
                let mut archive = archive.borrow_mut();
                // Checking after parsing is not a sandbox: the worker also has an address-space limit.
                if archive.len() > MAX_ENTRIES {
                    return Err(Error::Limit);
                }
                let mut named = Vec::new();
                for i in 0..archive.len() {
                    let item = archive.by_index_raw(i).map_err(|_| Error::Corrupt)?;
                    let raw = item.name_raw();
                    if raw.len() > ncat_protocol::MAX_PATH_BYTES {
                        return Err(Error::Limit);
                    }
                    if item.is_dir()
                        || item.is_symlink()
                        || raw.starts_with(b"__MACOSX/")
                        || raw
                            .rsplit(|c| *c == b'/')
                            .next()
                            .unwrap_or_default()
                            .starts_with(b"._")
                        || !image_hint(raw)
                    {
                        continue;
                    }
                    if item.encrypted() {
                        return Err(Error::Encrypted);
                    }
                    let page = Page {
                        key: PageKey::Zip(i as u64),
                        name: label(raw),
                    };
                    cost = cost
                        .checked_add(page.cost() + raw.len() as u64 * 2)
                        .ok_or(Error::Limit)?;
                    if cost > INDEX_BYTES / 2 {
                        return Err(Error::Limit);
                    }
                    named.push((raw.to_vec(), page));
                }
                named.sort_by(|a, b| {
                    natural_cmp(&a.0, &b.0).then_with(|| match (&a.1.key, &b.1.key) {
                        (PageKey::Zip(x), PageKey::Zip(y)) => x.cmp(y),
                        _ => unreachable!(),
                    })
                });
                pages = named.into_iter().map(|(_, page)| page).collect();
            }
        }
        checked_catalog(&pages)?;
        self.pages = pages;
        self.deferred = false;
        Ok(())
    }

    fn input(
        &self,
        key: &PageKey,
        output: &Path,
    ) -> Result<(Box<dyn ReadSeek>, PathBuf, Revision), Error> {
        if !self.pages.iter().any(|page| &page.key == key) {
            return Err(Error::Protocol);
        }
        match (&self.source, key) {
            (Source::Folder { .. }, PageKey::File(raw)) => {
                let path = raw.path();
                let file = File::open(&path).map_err(|_| Error::Io)?;
                let revision = Revision::of(&file)?;
                if revision.size > MAX_INPUT {
                    return Err(Error::Limit);
                }
                Ok((Box::new(LimitedReader::new(file)), path, revision))
            }
            (
                Source::Zip {
                    path,
                    revision,
                    archive,
                },
                PageKey::Zip(index),
            ) => {
                if &Revision::read(path)? != revision {
                    return Err(Error::Changed);
                }
                let mut archive = archive.borrow_mut();
                let mut item = archive
                    .by_index(usize::try_from(*index).map_err(|_| Error::Limit)?)
                    .map_err(|_| Error::Corrupt)?;
                if item.encrypted() {
                    return Err(Error::Encrypted);
                }
                if item.size() > MAX_INPUT {
                    return Err(Error::Limit);
                }
                // Anonymous, bounded disk spool: never an archive-controlled extraction path.
                let mut spool = tempfile::tempfile_in(output.parent().ok_or(Error::Protocol)?)
                    .map_err(|_| Error::Io)?;
                let read = io::copy(&mut Read::by_ref(&mut item).take(MAX_INPUT + 1), &mut spool)
                    .map_err(|_| Error::Corrupt)?;
                if read > MAX_INPUT {
                    return Err(Error::Limit);
                }
                // Force EOF/CRC validation, including implementations with untrusted size headers.
                let mut extra = [0];
                if item.read(&mut extra).map_err(|_| Error::Corrupt)? != 0 {
                    return Err(Error::Limit);
                }
                spool.seek(SeekFrom::Start(0)).map_err(|_| Error::Io)?;
                Ok((
                    Box::new(LimitedReader::new(spool)),
                    path.clone(),
                    revision.clone(),
                ))
            }
            _ => Err(Error::Protocol),
        }
    }

    pub fn decode(
        &self,
        key: &PageKey,
        output: &Path,
        max_width: u32,
        max_height: u32,
        workspace: u64,
        quarter_turns: u8,
    ) -> Result<FrameInfo, Error> {
        self.animation.borrow_mut().take();
        let start = Instant::now();
        if max_width == 0
            || max_height == 0
            || max_width > ncat_protocol::MAX_VIEW_DIMENSION
            || max_height > ncat_protocol::MAX_VIEW_DIMENSION
            || quarter_turns > 3
        {
            return Err(Error::Limit);
        }
        let (mut input, source_path, revision) = self.input(key, output)?;
        let metadata = metadata::extract(&mut *input);
        input.seek(SeekFrom::Start(0)).map_err(|_| Error::Io)?;
        let mut reader = ImageReader::new(BufReader::new(input))
            .with_guessed_format()
            .map_err(|_| Error::Corrupt)?;
        let format = reader.format().ok_or(Error::Unsupported)?;
        if !matches!(
            format,
            ImageFormat::Jpeg
                | ImageFormat::Png
                | ImageFormat::Bmp
                | ImageFormat::Gif
                | ImageFormat::WebP
        ) {
            return Err(Error::Unsupported);
        }
        let mut limits = image::Limits::default();
        limits.max_alloc = Some(workspace);
        let max_dimension = u32::try_from(MAX_PIXELS).map_err(|_| Error::Limit)?;
        limits.max_image_width = Some(max_dimension);
        limits.max_image_height = Some(max_dimension);
        reader.limits(limits);
        let mut decoder = reader.into_decoder().map_err(|_| Error::Corrupt)?;
        let (width, height) = decoder.dimensions();
        if u64::from(width) * u64::from(height) > MAX_PIXELS || width == 0 || height == 0 {
            return Err(Error::Limit);
        }
        // M1 intentionally uses conservative full-decode admission. JPEG reduced decode is a later PoC.
        let output_size = rgba_len(max_width, max_height)
            .ok_or(Error::Limit)?
            .min(rgba_len(width, height).ok_or(Error::Limit)?)
            .min(MAX_OUTPUT);
        let needed = decoder
            .total_bytes()
            .checked_mul(3)
            .and_then(|n| n.checked_add(output_size * 2 + ncat_protocol::DECODE_OVERHEAD))
            .ok_or(Error::Limit)?;
        if needed > workspace {
            return Err(Error::Limit);
        }
        let orientation = decoder.orientation().map_err(|_| Error::Corrupt)?;
        let view = RenderSpec {
            width: max_width,
            height: max_height,
            quarter_turns,
        };
        if matches!(
            format,
            ImageFormat::Gif | ImageFormat::Png | ImageFormat::WebP
        ) && let Some(mut session) =
            AnimationSession::start(self, key, output, format, orientation, view, workspace)?
        {
            drop(decoder);
            if Revision::read(&source_path)? != revision {
                return Err(Error::Changed);
            }
            session.metadata = metadata;
            *self.animation.borrow_mut() = Some(session);
            return self.animation_next(output)?.ok_or(Error::Corrupt);
        }
        let mut decoded = image::DynamicImage::from_decoder(decoder).map_err(|_| Error::Corrupt)?;
        decoded.apply_orientation(orientation);
        if Revision::read(&source_path)? != revision {
            return Err(Error::Changed);
        }
        let mut info = render(decoded, output, view, format, None, start)?;
        info.metadata = metadata;
        Ok(info)
    }

    pub fn animation_next(&self, output: &Path) -> Result<Option<FrameInfo>, Error> {
        let mut session = self.animation.borrow_mut().take().ok_or(Error::Protocol)?;
        let result = session.next(self, output);
        if result.is_ok() {
            *self.animation.borrow_mut() = Some(session);
        }
        result
    }

    pub fn animation_seek(&self, output: &Path, index: u32) -> Result<Option<FrameInfo>, Error> {
        if index >= ncat_protocol::MAX_ANIMATION_FRAMES {
            return Err(Error::Limit);
        }
        let mut session = self.animation.borrow_mut().take().ok_or(Error::Protocol)?;
        let result = session.seek(self, output, index);
        if result.is_ok() {
            *self.animation.borrow_mut() = Some(session);
        }
        result
    }
}

fn render(
    decoded: image::DynamicImage,
    output: &Path,
    view: RenderSpec,
    format: ImageFormat,
    animation: Option<ncat_protocol::AnimationInfo>,
    start: Instant,
) -> Result<FrameInfo, Error> {
    let mut decoded = match view.quarter_turns {
        1 => decoded.rotate90(),
        2 => decoded.rotate180(),
        3 => decoded.rotate270(),
        _ => decoded,
    };
    let (original_width, original_height) = (decoded.width(), decoded.height());
    let (mut w, mut h) =
        fit_size(original_width, original_height, view.width, view.height).ok_or(Error::Limit)?;
    if rgba_len(w, h).ok_or(Error::Limit)? > MAX_OUTPUT {
        let ratio = (MAX_OUTPUT as f64 / rgba_len(w, h).ok_or(Error::Limit)? as f64).sqrt();
        w = (f64::from(w) * ratio).floor().max(1.0) as u32;
        h = (f64::from(h) * ratio).floor().max(1.0) as u32;
    }
    if (w, h) != (original_width, original_height) {
        decoded = decoded.resize_exact(w, h, FilterType::Triangle);
    }
    let rgba = decoded.into_rgba8();
    let bytes = rgba_len(w, h).ok_or(Error::Limit)?;
    if bytes > MAX_OUTPUT || rgba.as_raw().len() as u64 != bytes {
        return Err(Error::Limit);
    }
    let mut out = ncat_platform::open_pixel_output(output).map_err(|_| Error::Io)?;
    if !out.metadata().map_err(|_| Error::Io)?.is_file() {
        return Err(Error::Protocol);
    }
    out.write_all(rgba.as_raw()).map_err(|_| Error::Io)?;
    out.flush().map_err(|_| Error::Io)?;
    Ok(FrameInfo {
        width: w,
        height: h,
        original_width,
        original_height,
        stride: u64::from(w) * 4,
        bytes,
        format: format!("{format:?}"),
        elapsed_us: start.elapsed().as_micros().min(u128::from(u64::MAX)) as u64,
        animation,
        metadata: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Generated, trusted fixtures only: these test codec/platform compatibility,
    // not the protected worker contract (which has a separate native suite).
    #[test]
    fn trusted_static_formats_archive_and_rotation() {
        let root = tempfile::tempdir().unwrap();
        let private = root.path().join("private");
        ncat_platform::create_private_directory(&private).unwrap();
        let output = private.join("pixels");
        let _owner = ncat_platform::create_private_file(&output).unwrap();
        let pixels = image::RgbaImage::from_pixel(12, 8, image::Rgba([220, 30, 60, 255]));
        for (extension, format) in [
            ("png", ImageFormat::Png),
            ("jpg", ImageFormat::Jpeg),
            ("bmp", ImageFormat::Bmp),
            ("webp", ImageFormat::WebP),
        ] {
            let source = root.path().join(format!("한글 page.{extension}"));
            image::DynamicImage::ImageRgba8(pixels.clone())
                .to_rgb8()
                .save_with_format(&source, format)
                .unwrap();
            let before = std::fs::read(&source).unwrap();
            let book = Book::open(&source).unwrap();
            let info = book
                .decode(&book.pages[0].key, &output, 100, 100, 32 * 1024 * 1024, 1)
                .unwrap();
            assert_eq!((info.original_width, info.original_height), (8, 12));
            assert!(info.valid());
            assert_eq!(std::fs::metadata(&output).unwrap().len(), info.bytes);
            assert_eq!(std::fs::read(&source).unwrap(), before);
        }
        let png = root.path().join("한글 page.png");
        let archive_path = root.path().join("한글 book.cbz");
        let mut archive = zip::ZipWriter::new(File::create(&archive_path).unwrap());
        for name in ["page10.png", "page2.png"] {
            archive
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            archive.write_all(&std::fs::read(&png).unwrap()).unwrap();
        }
        archive.finish().unwrap();
        let before = std::fs::read(&archive_path).unwrap();
        let book = Book::open(&archive_path).unwrap();
        assert_eq!(
            book.pages
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            ["page2.png", "page10.png"]
        );
        let info = book
            .decode(&book.pages[0].key, &output, 100, 100, 32 * 1024 * 1024, 0)
            .unwrap();
        assert_eq!((info.width, info.height), (12, 8));
        assert_eq!(std::fs::read(&archive_path).unwrap(), before);
    }

    #[test]
    fn trusted_gif_advances_and_seeks() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("한글 animation.gif");
        let mut encoder = image::codecs::gif::GifEncoder::new(File::create(&source).unwrap());
        for color in [[255, 0, 0, 255], [0, 255, 0, 255]] {
            encoder
                .encode_frame(image::Frame::from_parts(
                    image::RgbaImage::from_pixel(4, 3, image::Rgba(color)),
                    0,
                    0,
                    image::Delay::from_numer_denom_ms(100, 1),
                ))
                .unwrap();
        }
        drop(encoder);
        let mut original = fs::read(&source).unwrap();
        let trailer = original.len() - 1;
        original.splice(trailer..trailer, b"\x21\xfe\x05hello\0".iter().copied());
        fs::write(&source, &original).unwrap();
        let private = root.path().join("private");
        ncat_platform::create_private_directory(&private).unwrap();
        let output = private.join("pixels");
        let _owner = ncat_platform::create_private_file(&output).unwrap();
        let book = Book::open(&source).unwrap();
        let info = book
            .decode(&book.pages[0].key, &output, 100, 100, 32 * 1024 * 1024, 0)
            .unwrap();
        assert_eq!(info.metadata[0].value, "hello");
        assert_eq!(info.animation.unwrap().index, 0);
        let first = std::fs::read(&output).unwrap();
        let next = book.animation_next(&output).unwrap().unwrap();
        assert_eq!(next.animation.unwrap().index, 1);
        assert_eq!(next.metadata[0].value, "hello");
        assert_ne!(first, std::fs::read(&output).unwrap());
        let seek = book.animation_seek(&output, 0).unwrap().unwrap();
        assert_eq!(seek.animation.unwrap().index, 0);
        assert_eq!(first, std::fs::read(&output).unwrap());
        assert_eq!(fs::read(source).unwrap(), original);
    }
    #[test]
    fn actual_read_budget_survives_seek() {
        let mut input = LimitedReader {
            inner: io::Cursor::new(vec![1, 2, 3, 4]),
            remaining: 3,
        };
        let mut bytes = [0; 2];
        input.read_exact(&mut bytes).unwrap();
        input.seek(SeekFrom::Start(0)).unwrap();
        assert_eq!(input.read(&mut bytes).unwrap(), 1);
        assert!(input.read(&mut bytes).is_err());
    }
    #[test]
    fn control_characters_are_not_displayed() {
        assert_eq!(label(b"p\n1.png"), "p1.png");
        assert!(!label("a\u{202e}jpg".as_bytes()).contains('\u{202e}'));
    }
    #[cfg(unix)]
    #[test]
    fn folder_natural_order_and_non_utf8_identity() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["p10.png", "p2.png", "ignore.txt"] {
            File::create(dir.path().join(name)).unwrap();
        }
        let book = Book::open(dir.path()).unwrap();
        assert_eq!(
            book.pages
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            vec!["p2.png", "p10.png"]
        );
        let file = dir.path().join("p10.png");
        let mut book = Book::open(&file).unwrap();
        assert!(book.deferred);
        assert_eq!(book.pages.len(), 1);
        book.catalog().unwrap();
        assert_eq!(book.selected, 1);
    }
}
