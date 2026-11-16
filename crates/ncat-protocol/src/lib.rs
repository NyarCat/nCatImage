#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

pub const VERSION: u16 = 1;
pub const MAX_MESSAGE: usize = 256 * 1024;
pub const MAX_ENTRIES: usize = 100_000;
pub const INDEX_BYTES: u64 = 24 * 1024 * 1024;
pub const MAX_INPUT: u64 = 512 * 1024 * 1024;
pub const MAX_OUTPUT: u64 = 8 * 1024 * 1024;
pub const MAX_PIXELS: u64 = 150_000_000;
// Wire/security policy: shared by parent and worker, not user-overridable.
pub const MAX_PATH_BYTES: usize = 16 * 1024;
pub const MAX_LABEL_CHARS: usize = 512;
pub const MAX_LABEL_BYTES: usize = MAX_LABEL_CHARS * 4;
pub const PRESENTATION_COPIES: u64 = 3;
pub const MAX_VIEW_DIMENSION: u32 = 8192;
pub const PAGE_BATCH_BYTES: u64 = 30 * 1024;
pub const PAGE_BATCH_ENTRIES: usize = 128;
pub const WORKER_ADDRESS_SPACE: u64 = 1024 * 1024 * 1024;
pub const DECODE_OVERHEAD: u64 = 8 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
/// Lossless path bytes: the raw bytes on Unix; on Windows, the UTF-16 path as generalized
/// UTF-8 (WTF-8), which is plain UTF-8 for every well-formed name.
pub struct RawPath(pub Vec<u8>);
impl RawPath {
    #[cfg(unix)]
    pub fn new(path: &Path) -> Self {
        use std::os::unix::ffi::OsStrExt;
        Self(path.as_os_str().as_bytes().to_vec())
    }
    #[cfg(unix)]
    pub fn path(&self) -> PathBuf {
        use std::os::unix::ffi::OsStringExt;
        std::ffi::OsString::from_vec(self.0.clone()).into()
    }
    #[cfg(windows)]
    pub fn new(path: &Path) -> Self {
        use std::os::windows::ffi::OsStrExt;
        Self(wtf8::encode(path.as_os_str().encode_wide()))
    }
    /// Malformed bytes decode to U+FFFD, which can only name a path that does not exist.
    #[cfg(windows)]
    pub fn path(&self) -> PathBuf {
        use std::os::windows::ffi::OsStringExt;
        std::ffi::OsString::from_wide(&wtf8::decode(&self.0)).into()
    }
}

#[cfg(any(windows, test))]
mod wtf8 {
    pub fn encode(units: impl Iterator<Item = u16>) -> Vec<u8> {
        let mut bytes = Vec::new();
        for unit in char::decode_utf16(units) {
            match unit {
                Ok(c) => bytes.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes()),
                Err(e) => {
                    let u = e.unpaired_surrogate();
                    bytes.extend_from_slice(&[
                        0xE0 | (u >> 12) as u8,
                        0x80 | ((u >> 6) & 0x3F) as u8,
                        0x80 | (u & 0x3F) as u8,
                    ]);
                }
            }
        }
        bytes
    }
    pub fn decode(bytes: &[u8]) -> Vec<u16> {
        let mut units = Vec::with_capacity(bytes.len());
        let mut rest = bytes;
        while let Some(&lead) = rest.first() {
            if let [0xED, b1 @ 0xA0..=0xBF, b2 @ 0x80..=0xBF, ..] = *rest {
                units.push(0xD000 | (u16::from(b1 & 0x3F) << 6) | u16::from(b2 & 0x3F));
                rest = &rest[3..];
                continue;
            }
            let len = match lead {
                0x00..=0x7F => 1,
                0xC2..=0xDF => 2,
                0xE0..=0xEF => 3,
                0xF0..=0xF4 => 4,
                _ => 0,
            };
            match rest.get(..len).and_then(|s| std::str::from_utf8(s).ok()) {
                Some(s) if len > 0 => {
                    units.extend(s.encode_utf16());
                    rest = &rest[len..];
                }
                _ => {
                    units.push(0xFFFD);
                    rest = &rest[1..];
                }
            }
        }
        units
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PageKey {
    File(RawPath),
    Zip(u64),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Page {
    pub key: PageKey,
    pub name: String,
}
impl Page {
    pub fn cost(&self) -> u64 {
        let path = match &self.key {
            PageKey::File(p) => p.0.len(),
            PageKey::Zip(_) => 0,
        };
        (path + self.name.len() + 128) as u64
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Envelope<T> {
    pub version: u16,
    pub id: u64,
    pub body: T,
}
impl<T> Envelope<T> {
    pub fn new(id: u64, body: T) -> Self {
        Self {
            version: VERSION,
            id,
            body,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Request {
    Open {
        source: RawPath,
    },
    Catalog,
    Decode {
        key: PageKey,
        output: RawPath,
        max_width: u32,
        max_height: u32,
        workspace: u64,
        quarter_turns: u8,
    },
    Shutdown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCode {
    Io,
    Unsupported,
    Corrupt,
    Encrypted,
    Limit,
    Changed,
    Protocol,
    Worker,
    Timeout,
    Cancelled,
}
impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Io => "파일을 읽을 수 없습니다",
            Self::Unsupported => "아직 지원하지 않는 형식입니다",
            Self::Corrupt => "손상되었거나 읽을 수 없는 이미지입니다",
            Self::Encrypted => "암호화 압축은 지원하지 않습니다",
            Self::Limit => "크기 또는 메모리 한도를 초과했습니다",
            Self::Changed => "원본이 변경되었습니다. 다시 열어 주세요",
            Self::Protocol => "워커 응답이 올바르지 않습니다",
            Self::Worker => "워커가 종료되었습니다",
            Self::Timeout => "작업 제한 시간을 초과했습니다",
            Self::Cancelled => "작업이 취소되었습니다",
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FrameInfo {
    pub width: u32,
    pub height: u32,
    pub original_width: u32,
    pub original_height: u32,
    pub stride: u64,
    pub bytes: u64,
    pub format: String,
    pub elapsed_us: u64,
}
impl FrameInfo {
    pub fn valid(&self) -> bool {
        self.width > 0
            && self.height > 0
            && self.width <= MAX_VIEW_DIMENSION
            && self.height <= MAX_VIEW_DIMENSION
            && self.original_width > 0
            && self.original_height > 0
            && u64::from(self.original_width) * u64::from(self.original_height) <= MAX_PIXELS
            && self.stride == u64::from(self.width) * 4
            && self.stride.checked_mul(u64::from(self.height)) == Some(self.bytes)
            && self.bytes <= MAX_OUTPUT
            && self.format.len() <= 32
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Response {
    Opened {
        title: String,
        deferred: bool,
    },
    Pages {
        pages: Vec<Page>,
        selected: usize,
        done: bool,
    },
    Frame(FrameInfo),
    Error(ErrorCode),
    Bye,
}

pub fn write_message<T: Serialize>(writer: &mut impl Write, value: &Envelope<T>) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    if bytes.len() > MAX_MESSAGE {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "message limit"));
    }
    writer.write_all(&(bytes.len() as u32).to_le_bytes())?;
    writer.write_all(&bytes)?;
    writer.flush()
}

pub fn read_message<T: for<'de> Deserialize<'de>>(
    reader: &mut impl Read,
) -> io::Result<Option<Envelope<T>>> {
    let mut length = [0; 4];
    match reader.read(&mut length[..1])? {
        0 => return Ok(None),
        1 => {}
        _ => unreachable!(),
    }
    reader.read_exact(&mut length[1..])?;
    let length = u32::from_le_bytes(length) as usize;
    if length == 0 || length > MAX_MESSAGE {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "message limit"));
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes)?;
    let message: Envelope<T> = serde_json::from_slice(&bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid message"))?;
    if message.version != VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "protocol version",
        ));
    }
    Ok(Some(message))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wtf8_round_trips_utf16_including_unpaired_surrogates() {
        let samples: [Vec<u16>; 4] = [
            "p10.png".encode_utf16().collect(),
            "만화 第1巻 🐱.cbz".encode_utf16().collect(),
            vec![0x61, 0xD800, 0x62, 0xDFFF],
            vec![0xDBFF],
        ];
        for units in samples {
            let bytes = wtf8::encode(units.iter().copied());
            assert_eq!(wtf8::decode(&bytes), units);
        }
        assert_eq!(
            wtf8::encode("만화.png".encode_utf16()),
            "만화.png".as_bytes()
        );
        assert_eq!(wtf8::decode(&[0x61, 0xFF, 0xC3]), [0x61, 0xFFFD, 0xFFFD]);
    }
    #[test]
    fn bounded_and_versioned_messages() {
        let mut bytes = Vec::new();
        write_message(&mut bytes, &Envelope::new(7, Request::Catalog)).unwrap();
        let got = read_message::<Request>(&mut bytes.as_slice())
            .unwrap()
            .unwrap();
        assert_eq!(got.id, 7);
        assert!(read_message::<Request>(&mut (u32::MAX.to_le_bytes()).as_slice()).is_err());
        assert!(read_message::<Request>(&mut [1_u8, 0].as_slice()).is_err());
        assert!(
            read_message::<Request>(&mut [].as_slice())
                .unwrap()
                .is_none()
        );
        let mut bytes = Vec::new();
        write_message(
            &mut bytes,
            &Envelope {
                version: 0,
                id: 1,
                body: Request::Catalog,
            },
        )
        .unwrap();
        assert!(read_message::<Request>(&mut bytes.as_slice()).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn non_utf8_path_roundtrip() {
        let raw = RawPath(vec![b'/', 0xff, b'a']);
        let data = serde_json::to_vec(&raw).unwrap();
        let copy: RawPath = serde_json::from_slice(&data).unwrap();
        assert_eq!(RawPath::new(&copy.path()), raw);
    }
    #[cfg(windows)]
    #[test]
    fn non_unicode_path_roundtrip() {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let wide = [u16::from(b'C'), u16::from(b':'), 0x5C, 0xD800, 0x61];
        let path = PathBuf::from(std::ffi::OsString::from_wide(&wide));
        let raw = RawPath::new(&path);
        let data = serde_json::to_vec(&raw).unwrap();
        let copy: RawPath = serde_json::from_slice(&data).unwrap();
        assert_eq!(
            copy.path().as_os_str().encode_wide().collect::<Vec<_>>(),
            wide
        );
    }
    #[test]
    fn invalid_frame_rejected() {
        let frame = FrameInfo {
            width: 1,
            height: 1,
            original_width: 1,
            original_height: 1,
            stride: 4,
            bytes: 4,
            format: "Png".into(),
            elapsed_us: 0,
        };
        assert!(frame.valid());
        assert!(
            !FrameInfo {
                width: 8193,
                height: 1,
                original_width: 8193,
                original_height: 1,
                stride: 8193 * 4,
                bytes: 8193 * 4,
                ..frame.clone()
            }
            .valid()
        );
        assert!(
            !FrameInfo {
                stride: u64::MAX,
                ..frame.clone()
            }
            .valid()
        );
        assert!(
            !FrameInfo {
                bytes: MAX_OUTPUT + 1,
                ..frame
            }
            .valid()
        );
    }
}
