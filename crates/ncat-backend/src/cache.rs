//! Coordinator-owned, byte-bounded LRU. No filesystem work is done on the UI thread.
use crate::{Book, Frame, View};
use ncat_core::{Budget, Permit};
use ncat_protocol::{ErrorCode, PageKey};
use std::collections::VecDeque;
use std::fs;
use std::path::Path;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Revision {
    device: u64,
    inode: u64,
    size: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl Revision {
    pub fn differences(&self, now: &Self) -> String {
        format!(
            "stage=revision_comparison identity_changed={} size_changed={} mtime_changed={} ctime_changed={}",
            (self.device, self.inode) != (now.device, now.inode),
            self.size != now.size,
            self.modified != now.modified,
            self.changed != now.changed
        )
    }
    pub fn read(path: &Path) -> Result<Self, ErrorCode> {
        let m = fs::metadata(path).map_err(|_| ErrorCode::Io)?;
        if !m.is_file() {
            return Err(ErrorCode::Unsupported);
        }
        let stamp = ncat_core::FileStamp::of(&m);
        Ok(Self {
            device: stamp.device,
            inode: stamp.inode,
            size: m.len(),
            modified: stamp.modified,
            changed: stamp.changed,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    pub epoch: u64,
    pub page: PageKey,
    pub view: View,
}

impl Target {
    pub fn new(book: &Book, index: usize, view: View) -> Self {
        Self {
            epoch: book.epoch,
            page: book.pages[index].key.clone(),
            view,
        }
    }
    fn metadata_bytes(&self) -> u64 {
        256 + match &self.page {
            PageKey::File(path) => path.0.len() as u64,
            PageKey::Zip(_) => 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Key {
    pub target: Target,
    pub revision: Revision,
}

impl Key {
    pub fn read(book: &Book, index: usize, view: View) -> Result<Self, ErrorCode> {
        let target = Target::new(book, index, view);
        let revision = match &target.page {
            PageKey::File(path) => Revision::read(&path.path())?,
            PageKey::Zip(_) => {
                let now = Revision::read(&book.source.path())?;
                if book.source_revision.as_ref() != Some(&now) {
                    return Err(ErrorCode::Changed);
                }
                now
            }
        };
        Ok(Self { target, revision })
    }
}

struct Entry {
    key: Key,
    frame: Arc<Frame>,
    prefetched: bool,
    // Pixel reservations remain on Frame, shared with GBytes. Only key overhead is new.
    _metadata: Permit,
}

pub(crate) struct FrameCache {
    entries: VecDeque<Entry>,
    bytes: u64,
    capacity: u64,
    evictions: u64,
}

impl FrameCache {
    pub fn new(capacity: u64) -> Self {
        Self {
            entries: VecDeque::new(),
            bytes: 0,
            capacity,
            evictions: 0,
        }
    }
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
    pub fn evictions(&self) -> u64 {
        self.evictions
    }
    pub fn enabled(&self) -> bool {
        self.capacity > 0
    }
    pub fn contains(&self, key: &Key) -> bool {
        self.entries.iter().any(|e| &e.key == key)
    }
    pub fn get(&mut self, key: &Key) -> Option<(Arc<Frame>, bool)> {
        let position = self.entries.iter().position(|e| &e.key == key)?;
        let mut entry = self.entries.remove(position)?;
        let prefetched = entry.prefetched;
        entry.prefetched = false;
        let frame = entry.frame.clone();
        self.entries.push_back(entry);
        Some((frame, prefetched))
    }
    fn remove(&mut self, index: usize) {
        if let Some(entry) = self.entries.remove(index) {
            self.bytes -= entry.frame.info.bytes + entry._metadata.bytes();
            self.evictions += 1;
        }
    }
    pub fn evict_one(&mut self) -> bool {
        if self.entries.is_empty() {
            return false;
        }
        self.remove(0);
        true
    }
    pub fn clear(&mut self) {
        while self.evict_one() {}
    }
    pub fn remove_target(&mut self, target: &Target) {
        for index in (0..self.entries.len()).rev() {
            if &self.entries[index].key.target == target {
                self.remove(index);
            }
        }
    }
    pub fn discard_old_revisions(&mut self, key: &Key) {
        for index in (0..self.entries.len()).rev() {
            if self.entries[index].key.target == key.target
                && self.entries[index].key.revision != key.revision
            {
                self.remove(index);
            }
        }
    }
    pub fn insert(
        &mut self,
        key: Key,
        frame: Arc<Frame>,
        prefetched: bool,
        budget: &Budget,
    ) -> bool {
        let metadata_bytes = key.target.metadata_bytes();
        let cost = frame.info.bytes + metadata_bytes;
        if cost > self.capacity {
            return false;
        }
        self.remove_target(&key.target);
        // Also bound object/key overhead when frames are tiny; byte capacity is always authoritative.
        while self.bytes + cost > self.capacity
            || self.entries.len() >= crate::options::MAX_CACHE_ENTRIES
        {
            self.evict_one();
        }
        let Some(metadata) = budget.reserve(metadata_bytes) else {
            return false;
        };
        self.entries.push_back(Entry {
            key,
            frame,
            prefetched,
            _metadata: metadata,
        });
        self.bytes += cost;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ncat_protocol::{FrameInfo, RawPath};
    fn key(id: u8) -> Key {
        Key {
            target: Target {
                epoch: 1,
                page: PageKey::File(RawPath(vec![id])),
                view: View::default(),
            },
            revision: Revision {
                device: 1,
                inode: id as u64,
                size: 4,
                modified: (0, 0),
                changed: (0, 0),
            },
        }
    }
    fn frame(budget: &Budget) -> Arc<Frame> {
        Arc::new(Frame {
            pixels: vec![0; 4],
            info: FrameInfo {
                width: 1,
                height: 1,
                original_width: 1,
                original_height: 1,
                stride: 4,
                bytes: 4,
                format: "Png".into(),
                elapsed_us: 0,
            },
            _permit: budget.reserve(12).unwrap(),
        })
    }
    #[test]
    fn lru_is_byte_bounded_and_evicts_least_recent() {
        let budget = Budget::new(4096);
        let mut cache = FrameCache::new(530);
        assert!(cache.insert(key(1), frame(&budget), false, &budget));
        assert!(cache.insert(key(2), frame(&budget), true, &budget));
        assert!(cache.get(&key(1)).is_some());
        cache.insert(key(3), frame(&budget), false, &budget);
        assert!(!cache.contains(&key(2)));
        assert!(cache.contains(&key(1)));
        assert!(cache.bytes() <= 530);
        cache.clear();
        assert_eq!(budget.used(), 0);
    }
    #[test]
    fn cache_eviction_never_invalidates_a_display_lease() {
        let budget = Budget::new(4096);
        let mut cache = FrameCache::new(530);
        let live = frame(&budget);
        cache.insert(key(1), live.clone(), false, &budget);
        cache.clear();
        assert_eq!(budget.used(), 12);
        drop(live);
        assert_eq!(budget.used(), 0);
    }
    #[test]
    fn key_separates_epoch_revision_resolution_and_rotation() {
        let budget = Budget::new(4096);
        let mut cache = FrameCache::new(1024);
        let original = key(1);
        cache.insert(original.clone(), frame(&budget), true, &budget);
        let mut changed = original.clone();
        changed.target.epoch += 1;
        assert!(!cache.contains(&changed));
        changed = original.clone();
        changed.target.view.width += 1;
        assert!(!cache.contains(&changed));
        changed = original.clone();
        changed.target.view.quarter_turns = 1;
        assert!(!cache.contains(&changed));
        changed = original;
        changed.revision.changed.1 += 1;
        cache.discard_old_revisions(&changed);
        assert_eq!(cache.bytes(), 0);
        assert_eq!(budget.used(), 0);
    }
}
