//! Last normal window size only. Call load/save outside the GUI event loop.
use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WindowSize {
    pub width: i32,
    pub height: i32,
}

impl WindowSize {
    pub fn new(width: i32, height: i32) -> Option<Self> {
        ((1..=32768).contains(&width) && (1..=32768).contains(&height))
            .then_some(Self { width, height })
    }
}

fn absolute_env(key: &str) -> Option<PathBuf> {
    env::var_os(key)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

pub fn state_path() -> Option<PathBuf> {
    if cfg!(target_os = "linux") {
        let base = absolute_env("XDG_STATE_HOME")
            .or_else(|| absolute_env("HOME").map(|home| home.join(".local/state")))?;
        Some(base.join("ncatimage/window-state.json"))
    } else if cfg!(windows) {
        Some(absolute_env("LOCALAPPDATA")?.join("nCatImage/window-state.json"))
    } else {
        None
    }
}

pub fn load(path: &Path) -> Option<WindowSize> {
    const MAX_BYTES: u64 = 256;
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .ok()?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_BYTES {
        return None;
    }
    let size: WindowSize = serde_json::from_slice(&bytes).ok()?;
    WindowSize::new(size.width, size.height)
}

pub fn save(path: &Path, size: WindowSize) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("window state parent"))?;
    fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(&mut file, &size)?;
    file.write_all(b"\n")?;
    file.persist(path)?;
    Ok(())
}

/// Grow only the axes that need more room; never shrink an existing window.
pub fn startup_growth(
    image: (i32, i32),
    viewport: (i32, i32),
    available: (i32, i32),
) -> (i32, i32) {
    let growth =
        |needed: i32, current: i32, limit: i32| needed.min(limit).saturating_sub(current).max(0);
    (
        growth(image.0, viewport.0, available.0),
        growth(image.1, viewport.1, available.1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips_and_ignores_invalid_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app/window-state.json");
        assert_eq!(load(&path), None);
        let size = WindowSize::new(950, 625).unwrap();
        save(&path, size).unwrap();
        assert_eq!(load(&path), Some(size));
        let resized = WindowSize::new(800, 700).unwrap();
        save(&path, resized).unwrap();
        assert_eq!(load(&path), Some(resized));
        for invalid in [
            r#"{"width":0,"height":650}"#,
            r#"{"width":99999,"height":650}"#,
            r#"{"width":900,"height":650,"path":"not state"}"#,
            "broken",
        ] {
            fs::write(&path, invalid).unwrap();
            assert_eq!(load(&path), None);
        }
        fs::write(&path, " ".repeat(257)).unwrap();
        assert_eq!(load(&path), None);
    }

    #[test]
    fn growth_is_per_axis_and_bounded_by_available_space() {
        assert_eq!(startup_growth((600, 500), (900, 700), (1200, 800)), (0, 0));
        assert_eq!(
            startup_growth((600, 2000), (900, 700), (1200, 800)),
            (0, 100)
        );
        assert_eq!(
            startup_growth((1350, 800), (1000, 700), (900, 750)),
            (0, 50)
        );
    }
}
