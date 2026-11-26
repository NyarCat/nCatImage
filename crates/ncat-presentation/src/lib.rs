//! Toolkit-independent reader state and policies. No widgets, event loop or OS services.
#![forbid(unsafe_code)]

use ncat_core::{Direction, physical_next};
pub mod input;
pub mod metadata;
pub mod playback;
pub mod reader_bars;

pub fn reader_title(
    app: &str,
    page: Option<(usize, Option<usize>, &str)>,
    animation: playback::Playback,
    status: &str,
) -> String {
    use std::fmt::Write;
    let mut title = String::new();
    if let Some((index, total, name)) = page {
        let total = total.map_or("?".into(), |n| n.to_string());
        let _ = write!(title, "{index}/{total} · {name} · ");
    }
    if animation.active {
        let total = animation.total.map_or("?".into(), |n| n.to_string());
        let _ = write!(title, "프레임 {}/{} · ", animation.index + 1, total);
    }
    if !status.is_empty() {
        let _ = write!(title, "{status} — ");
    }
    title.push_str(app);
    title
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageKey {
    Next,
    Previous,
    Left,
    Right,
}

pub fn page_step(key: PageKey, direction: Direction) -> bool {
    match key {
        PageKey::Next => true,
        PageKey::Previous => false,
        PageKey::Left | PageKey::Right => physical_next(direction, key == PageKey::Right),
    }
}

pub fn adjacent(selected: usize, len: usize, forward: bool) -> Option<usize> {
    if len == 0 {
        return None;
    }
    Some(if forward {
        selected.saturating_add(1).min(len - 1)
    } else {
        selected.saturating_sub(1).min(len - 1)
    })
}

pub mod window_state;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReaderView {
    pub zoom: f64,
    pub fit: bool,
    pub quarter_turns: u8,
    pub original: (u32, u32),
}

impl Default for ReaderView {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            fit: false,
            quarter_turns: 0,
            original: (1, 1),
        }
    }
}

impl ReaderView {
    pub fn change_zoom(&mut self, factor: f64, viewport: (i32, i32), scale: f64) {
        if !factor.is_finite() || factor < 0.0 {
            return;
        }
        if factor == 0.0 {
            self.fit = true;
        } else {
            let current = self.display_scale(viewport, scale);
            self.fit = false;
            self.zoom = (current * factor).clamp(0.05, 16.0);
        }
    }

    pub fn rotate(&mut self, backwards: bool) {
        self.quarter_turns = (self.quarter_turns + if backwards { 3 } else { 1 }) % 4;
    }

    fn display_scale(&self, viewport: (i32, i32), scale: f64) -> f64 {
        if self.fit {
            (f64::from(viewport.0.max(1)) * scale / f64::from(self.original.0.max(1)))
                .min(f64::from(viewport.1.max(1)) * scale / f64::from(self.original.1.max(1)))
                .min(1.0)
        } else {
            self.zoom
        }
    }

    pub fn picture_size(&self, viewport: (i32, i32), scale: f64) -> (i32, i32) {
        let ratio = self.display_scale(viewport, scale) / scale;
        let size = |pixels: u32| (f64::from(pixels) * ratio).round().max(1.0) as i32;
        (size(self.original.0), size(self.original.1))
    }

    /// Show the requested full image size before viewport clipping or OS scaling.
    pub fn zoom_readout(&self, viewport: (i32, i32), scale: f64) -> String {
        let ratio = self.display_scale(viewport, scale);
        let percent = format!("{:.1}", ratio * 100.0);
        let percent = percent.strip_suffix(".0").unwrap_or(&percent);
        let pixels = |size| (f64::from(size) * ratio).round().max(1.0) as u32;
        format!(
            "{percent}%\n{} × {} px",
            pixels(self.original.0),
            pixels(self.original.1)
        )
    }

    pub fn picture_origin(
        &self,
        picture: (i32, i32),
        viewport: (i32, i32),
        pan: (i32, i32),
    ) -> (i32, i32) {
        let offset = |space: i32, size: i32, pan: i32| {
            let remaining = space - size;
            if remaining >= 0 {
                remaining / 2
            } else {
                pan.clamp(remaining, 0)
            }
        };
        (
            offset(viewport.0, picture.0, pan.0),
            offset(viewport.1, picture.1, pan.1),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_distinguishes_page_and_frame_without_playback_labels() {
        let mut animation = playback::Playback::default();
        assert_eq!(
            reader_title("Viewer", None, animation, "열기"),
            "열기 — Viewer"
        );
        animation.displayed(1, 0, 100);
        animation.paused = true;
        let page = Some((3, Some(9), "photo.gif"));
        assert_eq!(
            reader_title("Viewer", page, animation, ""),
            "3/9 · photo.gif · 프레임 2/? · Viewer"
        );
        animation.total = Some(6);
        assert!(reader_title("Viewer", page, animation, "GIF").contains("프레임 2/6"));
    }

    #[test]
    fn direction_only_changes_horizontal_input() {
        for direction in [Direction::LeftToRight, Direction::RightToLeft] {
            assert!(page_step(PageKey::Next, direction));
            assert!(!page_step(PageKey::Previous, direction));
        }
        assert!(page_step(PageKey::Right, Direction::LeftToRight));
        assert!(!page_step(PageKey::Right, Direction::RightToLeft));
    }

    #[test]
    fn navigation_is_bounded() {
        assert_eq!(adjacent(0, 0, true), None);
        assert_eq!(adjacent(0, 3, false), Some(0));
        assert_eq!(adjacent(2, 3, true), Some(2));
        assert_eq!(adjacent(0, 3, true), Some(1));
    }

    #[test]
    fn view_is_bounded_and_resettable() {
        let mut view = ReaderView::default();
        view.change_zoom(100.0, (1000, 800), 2.0);
        assert_eq!(view.zoom, 16.0);
        view.change_zoom(f64::NAN, (1000, 800), 2.0);
        assert_eq!(view.zoom, 16.0);
        view.change_zoom(0.0, (1000, 800), 2.0);
        assert!(view.fit);
        view.rotate(true);
        assert_eq!(view.quarter_turns, 3);
        view.rotate(false);
        assert_eq!(view.quarter_turns, 0);
    }

    #[test]
    fn zoom_readout_shows_full_dimensions_even_if_clipped() {
        let mut view = ReaderView {
            original: (1200, 800),
            ..ReaderView::default()
        };
        assert_eq!(view.zoom_readout((500, 400), 1.25), "100%\n1200 × 800 px");
        view.change_zoom(1.25, (500, 400), 1.0);
        assert_eq!(view.zoom_readout((500, 400), 1.0), "125%\n1500 × 1000 px");
        view.change_zoom(0.0, (500, 400), 1.0);
        assert_eq!(view.zoom_readout((500, 400), 1.0), "41.7%\n500 × 333 px");
    }

    #[test]
    fn native_view_centers_only_on_axes_that_fit() {
        let mut view = ReaderView {
            original: (1400, 900),
            ..ReaderView::default()
        };
        for (viewport, origin) in [
            ((600, 400), (0, 0)),
            ((1800, 1200), (200, 150)),
            ((600, 1200), (0, 150)),
            ((1800, 400), (200, 0)),
            ((1400, 900), (0, 0)),
        ] {
            let size = view.picture_size(viewport, 1.0);
            assert_eq!(size, (1400, 900));
            assert_eq!(view.picture_origin(size, viewport, (0, 0)), origin);
        }
        assert_eq!(
            view.picture_origin((1400, 900), (600, 1200), (-900, -100)),
            (-800, 150)
        );
        assert_eq!(view.picture_size((500, 400), 2.0), (700, 450));
        assert_eq!(view.picture_size((500, 400), 1.25), (1120, 720));
        view.change_zoom(0.0, (700, 450), 1.0);
        assert_eq!(view.picture_size((700, 450), 1.0), (700, 450));
        view.change_zoom(1.25, (700, 450), 1.0);
        assert_eq!(view.picture_size((500, 400), 1.0), (875, 563));
    }
}
