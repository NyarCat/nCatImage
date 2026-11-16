//! Bounded, path-redacted incident snapshots. Only the coordinator does disk I/O.
use std::{
    collections::VecDeque,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

const HISTORY_LINES: usize = 128;
const MAX_LINE_BYTES: usize = 1024;
const MAX_FILES: usize = 8;
const PREFIX: &str = "ncat-incident-";

/// Only typed, path-free data may cross the GUI diagnostic boundary.
#[derive(Clone, Copy, Debug)]
pub enum DropEvent {
    NetworkResolving {
        request: u64,
        protocol: NetworkProtocol,
    },
    NetworkResolved {
        request: u64,
        bridge: NetworkBridge,
    },
    NetworkFailed {
        request: u64,
        reason: NetworkFailure,
    },
    NetworkCancelled {
        request: u64,
    },
    Ready {
        wayland: bool,
    },
    Entered {
        file_list: bool,
        file: bool,
        uri_list: bool,
        portal: bool,
        actions: u32,
        wayland: bool,
    },
    Accepted {
        compatible: bool,
    },
    Left,
    Received {
        file_list: bool,
        items: usize,
    },
    Rejected(DropRejection),
    OpenQueued {
        generation: u64,
    },
    Displayed {
        generation: u64,
    },
    OpenFailed {
        generation: u64,
        code: ncat_protocol::ErrorCode,
    },
    Superseded {
        generation: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkProtocol {
    Smb,
    Ftp,
    Sftp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkBridge {
    KioFuse,
    Gvfs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkFailure {
    UnsupportedScheme,
    InvalidUri,
    EmbeddedPassword,
    Unavailable,
    NoLocalPath,
    Permission,
    NotFound,
    Connection,
    Timeout,
    Cancelled,
    InvalidReply,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropRejection {
    UnsupportedValue,
    Empty,
    MultipleItems,
    NonLocal,
}

pub(crate) struct Diagnostics {
    directory: PathBuf,
    history: VecDeque<String>,
    started: Instant,
    sequence: u64,
    warned: bool,
}

impl Diagnostics {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            history: VecDeque::new(),
            started: Instant::now(),
            sequence: 0,
            warned: false,
        }
    }
    // Callers supply only fixed stage labels, numeric counters and typed error codes.
    // Never pass Debug for Book/Job/Page/Path or arbitrary error chains here.
    pub fn record(&mut self, event: String) {
        let mut line = format!("elapsed_us={} {event}", self.started.elapsed().as_micros());
        if line.len() > MAX_LINE_BYTES {
            let mut end = MAX_LINE_BYTES;
            while !line.is_char_boundary(end) {
                end -= 1;
            }
            line.truncate(end);
        }
        if self.history.len() == HISTORY_LINES {
            self.history.pop_front();
        }
        self.history.push_back(line);
    }
    pub fn incident(&mut self) {
        if self.write_snapshot().is_err() && !self.warned {
            self.warned = true;
            eprintln!("nCatImage: could not write diagnostic log (log directory unavailable)");
        }
    }
    fn write_snapshot(&mut self) -> io::Result<()> {
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut builder = fs::DirBuilder::new();
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        // Windows: files inherit the directory ACL, which is per-user under the profile.
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
            builder.mode(0o700);
            options.mode(0o600);
        }
        match builder.create(&self.directory) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        if !fs::symlink_metadata(&self.directory)?.is_dir() {
            return Err(io::Error::other("not a directory"));
        }
        let mut files = Vec::new();
        for entry in fs::read_dir(&self.directory)? {
            let entry = entry?;
            if entry.file_type()?.is_file() && managed_name(&entry.path()) {
                files.push(entry.path());
            }
        }
        files.sort();
        // These names are reserved for this logger; never touch other files or symlinks.
        let remove = files.len().saturating_add(1).saturating_sub(MAX_FILES);
        for path in files.into_iter().take(remove) {
            fs::remove_file(path)?;
        }
        self.sequence += 1;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let name = format!(
            "{PREFIX}{timestamp}-{}-{}.log",
            std::process::id(),
            self.sequence
        );
        let mut file = options.open(self.directory.join(name))?;
        writeln!(
            file,
            "ncat-diagnostics-v1 unix_ns={timestamp} pid={} backend_version={} retention_files={MAX_FILES}",
            std::process::id(),
            env!("CARGO_PKG_VERSION")
        )?;
        for line in &self.history {
            writeln!(file, "{line}")?;
        }
        file.flush()
    }
}

fn managed_name(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let Some(parts) = name
        .strip_prefix(PREFIX)
        .and_then(|name| name.strip_suffix(".log"))
    else {
        return false;
    };
    let parts: Vec<_> = parts.split('-').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn drop_snapshots_contain_stages_without_source_data() {
        let root = tempfile::tempdir().unwrap();
        let mut log = Diagnostics::new(root.path().join("log"));
        for event in [
            DropEvent::Ready { wayland: true },
            DropEvent::Received {
                file_list: true,
                items: 1,
            },
            DropEvent::OpenQueued { generation: 7 },
            DropEvent::Displayed { generation: 7 },
        ] {
            log.record(format!("stage=drop event={event:?}"));
        }
        log.write_snapshot().unwrap();
        let file = fs::read_dir(root.path().join("log"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let text = fs::read_to_string(file).unwrap();
        assert!(text.contains("Ready { wayland: true }"));
        assert!(text.contains("Received { file_list: true, items: 1 }"));
        assert!(text.contains("Displayed { generation: 7 }"));
        assert!(!text.contains("file://"));
        assert!(!text.contains(".cbz"));
    }
    #[test]
    fn lazy_bounded_private_snapshots_keep_recent_context_and_other_files() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("log");
        let mut log = Diagnostics::new(directory.clone());
        for i in 0..200 {
            log.record(format!("stage=request generation={i}"));
        }
        assert!(!directory.exists());
        log.write_snapshot().unwrap();
        fs::write(directory.join("user-notes.txt"), "keep").unwrap();
        fs::write(directory.join("ncat-incident-custom.log"), "keep").unwrap();
        for _ in 0..12 {
            log.write_snapshot().unwrap();
        }
        let files: Vec<_> = fs::read_dir(&directory)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| managed_name(p))
            .collect();
        assert_eq!(files.len(), MAX_FILES);
        let text = fs::read_to_string(&files[0]).unwrap();
        assert!(text.contains("generation=199"));
        assert!(!text.contains("generation=0\n"));
        assert!(text.len() <= (HISTORY_LINES + 1) * (MAX_LINE_BYTES + 1));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&files[0]).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert!(directory.join("user-notes.txt").exists());
        assert!(directory.join("ncat-incident-custom.log").exists());
    }
    #[cfg(unix)]
    #[test]
    fn rejects_symlink_directory_and_log_failure_is_nonfatal() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        fs::create_dir(&target).unwrap();
        let link = root.path().join("log");
        symlink(&target, &link).unwrap();
        let mut log = Diagnostics::new(link);
        log.incident();
        assert!(log.warned);
        assert_eq!(fs::read_dir(target).unwrap().count(), 0);
    }
}
