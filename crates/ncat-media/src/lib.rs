//! Worker-only media implementation. Never linked into the GUI/backend.
#![forbid(unsafe_code)]

use image::{ImageDecoder, ImageFormat, ImageReader, imageops::FilterType};
use ncat_core::{fit_size, natural_cmp, rgba_len};
use ncat_protocol::{
    ErrorCode as Error, FrameInfo, INDEX_BYTES, MAX_ENTRIES, MAX_INPUT, MAX_OUTPUT, MAX_PIXELS,
    Page, PageKey, RawPath,
};
use std::cell::RefCell;
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

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

#[derive(Clone, Debug, PartialEq, Eq)]
struct Revision {
    dev: u64,
    ino: u64,
    len: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}
impl Revision {
    fn of(file: &File) -> Result<Self, Error> {
        Self::metadata(&file.metadata().map_err(|_| Error::Io)?)
    }
    fn path(path: &Path) -> Result<Self, Error> {
        Self::metadata(&fs::metadata(path).map_err(|_| Error::Io)?)
    }
    fn metadata(m: &fs::Metadata) -> Result<Self, Error> {
        if !m.is_file() {
            return Err(Error::Unsupported);
        }
        let stamp = ncat_core::FileStamp::of(m);
        Ok(Self {
            dev: stamp.device,
            ino: stamp.inode,
            len: m.len(),
            mtime: stamp.modified,
            ctime: stamp.changed,
        })
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
    source: Source,
    pub pages: Vec<Page>,
    pub selected: usize,
    pub title: String,
    pub deferred: bool,
}

fn image_hint(name: &[u8]) -> bool {
    let ext = name.rsplit(|c| *c == b'.').next().unwrap_or_default();
    [b"jpg".as_slice(), b"jpeg", b"png", b"bmp", b"gif", b"webp"]
        .iter()
        .any(|x| ext.eq_ignore_ascii_case(x))
}

fn label(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .filter(|c| {
            !c.is_control() && !matches!(*c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .take(ncat_protocol::MAX_LABEL_CHARS)
        .collect()
}

/// ZIP entry names carry no reliable charset: only flagged names are UTF-8, and Korean
/// or Japanese Windows tools write unflagged CP949/Shift_JIS. Flagged names are always
/// valid UTF-8 and legacy multibyte names almost never are, so try UTF-8 first.
fn entry_label(raw: &[u8]) -> String {
    if std::str::from_utf8(raw).is_ok() {
        return label(raw);
    }
    for encoding in [encoding_rs::EUC_KR, encoding_rs::SHIFT_JIS] {
        if let Some(name) = encoding.decode_without_bom_handling_and_without_replacement(raw) {
            return label(name.as_bytes());
        }
    }
    label(raw)
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
        if path.as_os_str().as_encoded_bytes().len() > ncat_protocol::MAX_PATH_BYTES {
            return Err(Error::Limit);
        }
        let meta = fs::metadata(path).map_err(|_| Error::Io)?;
        if meta.is_dir() {
            let mut book = Self {
                source: Source::Folder {
                    root: path.into(),
                    initial: None,
                },
                pages: Vec::new(),
                selected: 0,
                title: label(
                    path.file_name()
                        .unwrap_or(path.as_os_str())
                        .as_encoded_bytes(),
                ),
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
                source: Source::Zip {
                    path: path.into(),
                    revision,
                    archive: RefCell::new(archive),
                },
                pages: Vec::new(),
                selected: 0,
                title: label(
                    path.file_name()
                        .unwrap_or(path.as_os_str())
                        .as_encoded_bytes(),
                ),
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
        // Spell the initial page exactly as the folder scan will (`root.join(name)`), so
        // that `a.png` vs `./a.png`, or `C:/x/a.png` vs `C:/x\a.png`, is not listed twice.
        let path = &match path.file_name() {
            Some(name) => root.join(name),
            None => path.to_path_buf(),
        };
        Ok(Self {
            source: Source::Folder {
                root: root.into(),
                initial: Some(path.into()),
            },
            pages: vec![Page {
                key: PageKey::File(RawPath::new(path)),
                name: label(
                    path.file_name()
                        .unwrap_or(path.as_os_str())
                        .as_encoded_bytes(),
                ),
            }],
            selected: 0,
            title: label(
                root.file_name()
                    .unwrap_or(root.as_os_str())
                    .as_encoded_bytes(),
            ),
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
                        || !image_hint(name.as_encoded_bytes())
                    {
                        continue;
                    }
                    let page = Page {
                        key: PageKey::File(RawPath::new(&entry.path())),
                        name: label(name.as_encoded_bytes()),
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
                            name: label(
                                initial
                                    .file_name()
                                    .unwrap_or(initial.as_os_str())
                                    .as_encoded_bytes(),
                            ),
                        });
                    }
                }
                pages.sort_by(|a, b| match (&a.key, &b.key) {
                    (PageKey::File(a), PageKey::File(b)) => natural_cmp(&a.0, &b.0),
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
                if &Revision::path(path)? != revision {
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
                        name: entry_label(raw),
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
                if revision.len > MAX_INPUT {
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
                if &Revision::path(path)? != revision {
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
        let start = Instant::now();
        if max_width == 0
            || max_height == 0
            || max_width > ncat_protocol::MAX_VIEW_DIMENSION
            || max_height > ncat_protocol::MAX_VIEW_DIMENSION
            || quarter_turns > 3
        {
            return Err(Error::Limit);
        }
        let (input, source_path, revision) = self.input(key, output)?;
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
        let mut decoded = image::DynamicImage::from_decoder(decoder).map_err(|_| Error::Corrupt)?;
        decoded.apply_orientation(orientation);
        let mut decoded = match quarter_turns {
            1 => decoded.rotate90(),
            2 => decoded.rotate180(),
            3 => decoded.rotate270(),
            _ => decoded,
        };
        let (original_width, original_height) = (decoded.width(), decoded.height());
        let (mut w, mut h) =
            fit_size(original_width, original_height, max_width, max_height).ok_or(Error::Limit)?;
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
        if Revision::path(&source_path)? != revision {
            return Err(Error::Changed);
        }
        let mut out = open_output(output).map_err(|_| Error::Io)?;
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
        })
    }
}

/// Open the coordinator-created frame file for writing, never creating it and never
/// following a final symlink. The caller still checks that it opened a regular file.
#[cfg(unix)]
fn open_output(output: &Path) -> io::Result<File> {
    let fd = rustix::fs::open(
        output,
        rustix::fs::OFlags::WRONLY | rustix::fs::OFlags::TRUNC | rustix::fs::OFlags::NOFOLLOW,
        rustix::fs::Mode::empty(),
    )?;
    Ok(File::from(fd))
}
#[cfg(windows)]
fn open_output(output: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    // A reparse point (symlink/junction) is opened as itself and then fails is_file().
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(output)
}

#[cfg(test)]
mod tests {
    use super::*;
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
    #[test]
    fn legacy_zip_entry_names_are_decoded() {
        // "만화/가나.png" as written by Korean Windows archivers (CP949, no UTF-8 flag).
        let cp949 = b"\xb8\xb8\xc8\xad/\xb0\xa1\xb3\xaa.png";
        assert_eq!(entry_label(cp949), "만화/가나.png");
        assert_eq!(entry_label("만화/가나.png".as_bytes()), "만화/가나.png");
        // "ソ.png" in Shift_JIS has a trail byte CP949 rejects.
        assert_eq!(entry_label(b"\x83\x5c.png"), "ソ.png");
        assert_eq!(entry_label(b"\xff\xff.png"), "\u{fffd}\u{fffd}.png");
    }
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
