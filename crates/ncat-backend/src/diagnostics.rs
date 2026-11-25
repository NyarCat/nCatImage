//! Bounded, path-redacted diagnostics retained only for the current run.
use std::{collections::VecDeque, time::Instant};

const HISTORY_LINES: usize = 128;
const MAX_LINE_BYTES: usize = 1024;

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
    history: VecDeque<String>,
    started: Instant,
}

impl Diagnostics {
    pub fn new() -> Self {
        Self {
            history: VecDeque::new(),
            started: Instant::now(),
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
    pub fn snapshot(&self) -> String {
        self.history
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_snapshots_contain_stages_without_source_data() {
        let mut log = Diagnostics::new();
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
        let text = log.snapshot();
        assert!(text.contains("Ready { wayland: true }"));
        assert!(text.contains("Received { file_list: true, items: 1 }"));
        assert!(text.contains("Displayed { generation: 7 }"));
        assert!(!text.contains("file://"));
        assert!(!text.contains(".cbz"));
    }
    #[test]
    fn history_keeps_only_recent_bounded_utf8_lines() {
        let mut log = Diagnostics::new();
        assert!(log.snapshot().is_empty());
        for i in 0..200 {
            log.record(format!("stage=request generation={i}"));
        }
        log.record("한".repeat(MAX_LINE_BYTES));
        let text = log.snapshot();
        assert_eq!(text.lines().count(), HISTORY_LINES);
        assert!(text.lines().next().unwrap().ends_with("generation=73"));
        assert!(text.contains("generation=199\n"));
        let last = text.lines().last().unwrap();
        assert!((MAX_LINE_BYTES - 2..=MAX_LINE_BYTES).contains(&last.len()));
        assert!(last.ends_with('한'));
        assert!(Diagnostics::new().snapshot().is_empty());
    }
}
