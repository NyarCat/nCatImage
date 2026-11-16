#![forbid(unsafe_code)]

use std::cmp::Ordering;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering as AtomicOrdering},
};

pub const MIB: u64 = 1024 * 1024;
pub const DEFAULT_BUDGET: u64 = 192 * MIB;

/// Locale-independent natural ordering, without integer parsing or allocation.
/// Exact bytes break numeric ties (including leading zeroes).
pub fn natural_cmp(a: &[u8], b: &[u8]) -> Ordering {
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i].is_ascii_digit() && b[j].is_ascii_digit() {
            let (start_i, start_j) = (i, j);
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let x = &a[start_i..i];
            let y = &b[start_j..j];
            let x = &x[x.iter().position(|c| *c != b'0').unwrap_or(x.len())..];
            let y = &y[y.iter().position(|c| *c != b'0').unwrap_or(y.len())..];
            let order = x.len().cmp(&y.len()).then_with(|| x.cmp(y));
            if order != Ordering::Equal {
                return order;
            }
        } else {
            let order = a[i].cmp(&b[j]);
            if order != Ordering::Equal {
                return order;
            }
            i += 1;
            j += 1;
        }
    }
    (a.len() - i).cmp(&(b.len() - j)).then_with(|| a.cmp(b))
}

pub fn rgba_len(width: u32, height: u32) -> Option<u64> {
    if width == 0 || height == 0 {
        return None;
    }
    u64::from(width)
        .checked_mul(u64::from(height))?
        .checked_mul(4)
}

pub fn fit_size(width: u32, height: u32, max_width: u32, max_height: u32) -> Option<(u32, u32)> {
    if [width, height, max_width, max_height].contains(&0) {
        return None;
    }
    let scale = (f64::from(max_width) / f64::from(width))
        .min(f64::from(max_height) / f64::from(height))
        .min(1.0);
    Some((
        (f64::from(width) * scale).floor().max(1.0) as u32,
        (f64::from(height) * scale).floor().max(1.0) as u32,
    ))
}

#[derive(Clone, Debug)]
pub struct Budget(Arc<BudgetInner>);
#[derive(Debug)]
struct BudgetInner {
    limit: u64,
    used: AtomicU64,
}

impl Budget {
    pub fn new(limit: u64) -> Self {
        Self(Arc::new(BudgetInner {
            limit,
            used: AtomicU64::new(0),
        }))
    }
    pub fn used(&self) -> u64 {
        self.0.used.load(AtomicOrdering::Acquire)
    }
    pub fn limit(&self) -> u64 {
        self.0.limit
    }
    pub fn reserve(&self, bytes: u64) -> Option<Permit> {
        self.0
            .used
            .fetch_update(AtomicOrdering::AcqRel, AtomicOrdering::Acquire, |used| {
                used.checked_add(bytes).filter(|next| *next <= self.0.limit)
            })
            .ok()?;
        Some(Permit {
            budget: self.clone(),
            bytes,
        })
    }
}

#[derive(Debug)]
pub struct Permit {
    budget: Budget,
    bytes: u64,
}
impl Permit {
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
    pub fn shrink_to(&mut self, bytes: u64) {
        assert!(bytes <= self.bytes);
        self.budget
            .0
            .used
            .fetch_sub(self.bytes - bytes, AtomicOrdering::AcqRel);
        self.bytes = bytes;
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.budget
            .0
            .used
            .fetch_sub(self.bytes, AtomicOrdering::AcqRel);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    LeftToRight,
    RightToLeft,
}

/// Page indexes in logical order. Physical placement is a separate operation.
pub fn spreads(landscape: &[bool], cover: bool, double: bool) -> Vec<Vec<usize>> {
    let mut result = Vec::new();
    let mut i = 0;
    while i < landscape.len() {
        if !double || (cover && i == 0) || landscape[i] {
            result.push(vec![i]);
            i += 1;
        } else if i + 1 < landscape.len() && !landscape[i + 1] {
            result.push(vec![i, i + 1]);
            i += 2;
        } else {
            result.push(vec![i]);
            i += 1;
        }
    }
    result
}

pub fn physical_next(direction: Direction, right_key: bool) -> bool {
    (direction == Direction::LeftToRight) == right_key
}

/// File identity and change stamps used to detect a replaced or rewritten source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileStamp {
    pub device: u64,
    pub inode: u64,
    pub modified: (i64, i64),
    pub changed: (i64, i64),
}
impl FileStamp {
    #[cfg(unix)]
    pub fn of(m: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            device: m.dev(),
            inode: m.ino(),
            modified: (m.mtime(), m.mtime_nsec()),
            changed: (m.ctime(), m.ctime_nsec()),
        }
    }
    /// Stable std exposes no file ID on Windows, so identity is left zero. Size plus the
    /// 100ns write and creation times still catch in-place rewrites and most replacements;
    /// creation time stands in for ctime.
    #[cfg(windows)]
    pub fn of(m: &std::fs::Metadata) -> Self {
        use std::os::windows::fs::MetadataExt;
        Self {
            device: 0,
            inode: 0,
            modified: (m.last_write_time() as i64, 0),
            changed: (m.creation_time() as i64, 0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn natural_large_numbers_and_ties() {
        assert!(natural_cmp(b"page2.png", b"page10.png").is_lt());
        assert!(natural_cmp(b"x99999999999999999999", b"x100000000000000000000").is_lt());
        assert_ne!(natural_cmp(b"01", b"1"), Ordering::Equal);
        assert_eq!(natural_cmp(b"000", b"000"), Ordering::Equal);
        assert!(natural_cmp(b"A", b"a").is_lt());
    }
    #[test]
    fn sort_is_transitive_on_small_corpus() {
        let values = [
            b"0".as_slice(),
            b"00",
            b"1",
            b"01",
            b"2",
            b"10",
            b"a2",
            b"a02",
            b"a10",
        ];
        for a in values {
            for b in values {
                for c in values {
                    if natural_cmp(a, b).is_le() && natural_cmp(b, c).is_le() {
                        assert!(natural_cmp(a, c).is_le());
                    }
                }
            }
        }
    }
    #[test]
    fn checked_sizes_and_fit() {
        assert_eq!(rgba_len(0, 1), None);
        assert_eq!(rgba_len(u32::MAX, u32::MAX), None);
        assert_eq!(fit_size(2000, 3000, 1000, 1000), Some((666, 1000)));
        assert_eq!(fit_size(10, 20, 100, 100), Some((10, 20)));
    }
    #[test]
    fn last_owner_holds_budget() {
        let budget = Budget::new(10);
        let lease = Arc::new(budget.reserve(10).unwrap());
        let second = lease.clone();
        drop(lease);
        assert!(budget.reserve(1).is_none());
        drop(second);
        assert_eq!(budget.used(), 0);
        assert!(budget.reserve(u64::MAX).is_none());
    }
    #[test]
    fn budget_reservation_is_atomic() {
        let budget = Budget::new(100);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let budget = budget.clone();
                scope.spawn(move || {
                    for _ in 0..1000 {
                        if let Some(_permit) = budget.reserve(11) {
                            assert!(budget.used() <= 100);
                        }
                    }
                });
            }
        });
        assert_eq!(budget.used(), 0);
    }
    #[test]
    fn cover_and_landscape_spreads() {
        assert_eq!(
            spreads(
                &[false, false, false, true, false, false, false],
                true,
                true
            ),
            vec![vec![0], vec![1, 2], vec![3], vec![4, 5], vec![6]]
        );
        assert_eq!(
            spreads(&[false, true, false], false, true),
            vec![vec![0], vec![1], vec![2]]
        );
        assert!(physical_next(Direction::RightToLeft, false));
    }
}
