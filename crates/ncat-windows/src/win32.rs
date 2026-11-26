//! Win32 boundary: HWNDs stay on the UI thread, callbacks borrow through RefCell
//! to tolerate synchronous control notifications. Pixel staging runs off-thread.
use ncat_backend::{Backend, BackendOptions, Book, Event, Frame, View, WorkerCommand};
use ncat_presentation::{
    ReaderView, adjacent,
    input::{self, Command, Key, Modifiers},
    playback::{Playback, SPEEDS},
    reader_bars::{Bar, ReaderBars},
    window_state::{self, WindowSize},
};
use std::{
    cell::{Cell, RefCell},
    io,
    path::PathBuf,
    ptr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{
        ERROR_NOT_ENOUGH_QUOTA, GetLastError, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
    },
    Graphics::Gdi::*,
    System::{Com::*, LibraryLoader::GetModuleHandleW},
    UI::{
        Controls::{Dialogs::*, EM_SETLIMITTEXT, WM_MOUSELEAVE},
        HiDpi::{
            AreDpiAwarenessContextsEqual, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            DPI_AWARENESS_PER_MONITOR_AWARE, GetAwarenessFromDpiAwarenessContext, GetDpiForWindow,
            GetThreadDpiAwarenessContext, GetWindowDpiAwarenessContext,
            SetProcessDpiAwarenessContext,
        },
        Input::KeyboardAndMouse::*,
        Shell::*,
        WindowsAndMessaging::*,
    },
};

const RESULT: u32 = WM_APP + 1;
const VIEWPORT_CHANGED: u32 = WM_APP + 2;
const VIEWPORT_MINIMIZED: usize = 1;
const VIEWPORT_RESIZE_FINISHED: usize = 2;
const LIVE_RESIZE_TEST: u32 = WM_APP + 3;
const ANIMATION: usize = 1;
const SHUTDOWN: usize = 2;
const DEADLINE: usize = 3;
const RESIZE_TEST: usize = 5;
const METADATA_TEST: usize = 6;
const GLASS_SETTLE: usize = 7;
const TOP_SIZE: (i32, i32) = (306, 44);
const BOTTOM_SIZE: (i32, i32) = (252, 44);
const ZOOM_READOUT_WIDTH: i32 = 136;
const BASE_DPI: u32 = 96;
const ANIMATION_SIZE: (i32, i32) = (318, 44);
const OPEN: u16 = 101;
const FOLDER: u16 = 102;
const CONNECT: u16 = 104;
const PREVIOUS: u16 = 105;
const PAGE: u16 = 106;
const PAGE_GO: u16 = 107;
const NEXT: u16 = 108;
const DIRECTION: u16 = 109;
const SMALLER: u16 = 110;
const FIT: u16 = 111;
const LARGER: u16 = 112;
const ROTATE: u16 = 113;
const FULLSCREEN: u16 = 114;
const FRAME_PREVIOUS: u16 = 115;
const FRAME_NEXT: u16 = 116;
const PLAY: u16 = 117;
const SPEED: u16 = 118;
const FRAME_NUMBER: u16 = 119;
const FRAME_GO: u16 = 120;
const TOP_PIN: u16 = 123;
const BOTTOM_PIN: u16 = 124;
const METADATA: u16 = 125;
const VIEW_LOG: u16 = 126;
const COPY_METADATA: u16 = 127;
const INFO_SIZE: (i32, i32) = (52, 44);
const METADATA_TOOLBAR_HEIGHT: i32 = 36;

fn control_bar(id: u16) -> Bar {
    match id {
        PREVIOUS | PAGE | PAGE_GO | NEXT | DIRECTION | TOP_PIN => Bar::Top,
        _ => Bar::Bottom,
    }
}

fn animation_control(id: u16) -> bool {
    matches!(
        id,
        FRAME_PREVIOUS | FRAME_NEXT | PLAY | SPEED | FRAME_NUMBER | FRAME_GO
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Smoke {
    None,
    Window,
    Resize,
    ResizeFrame,
    MetadataWindow,
    MetadataFrame,
    Frame,
    Animation,
    Navigation,
    ModalLoad,
}

struct Prepared {
    event: Event,
    pixels: Option<Vec<u8>>,
}
struct Display {
    frame: Arc<Frame>,
    bgra: Vec<u8>,
}

// One UI-thread-owned surface. Round up to reuse it across nearby sizes;
// cap the extra 32-bpp storage at 32 MiB, with a no-buffer fallback.
struct PaintBuffer {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    width: i32,
    height: i32,
}
impl PaintBuffer {
    unsafe fn ensure(slot: &mut Option<Self>, dc: HDC, width: i32, height: i32) {
        if slot
            .as_ref()
            .is_some_and(|b| b.width >= width && b.height >= height)
        {
            return;
        }
        *slot = None;
        let round = |n: i32| ((n as i64 + 127) / 128) * 128;
        let (width, height) = (round(width), round(height));
        if width * height > 32 * 1024 * 1024 / 4 {
            return;
        }
        let (width, height) = (width as i32, height as i32);
        // SAFETY: the bitmap and memory DC are paired on the UI thread; the
        // DIB bits are never accessed outside GDI and the bitmap is deselected on drop.
        unsafe {
            let memory = CreateCompatibleDC(dc);
            if memory.is_null() {
                return;
            }
            let mut bits = ptr::null_mut();
            let bitmap = CreateDIBSection(
                dc,
                &bitmap_info(width, height),
                DIB_RGB_COLORS,
                &mut bits,
                ptr::null_mut(),
                0,
            );
            if !bitmap.is_null() {
                let previous = SelectObject(memory, bitmap);
                if !previous.is_null() {
                    *slot = Some(Self {
                        dc: memory,
                        bitmap,
                        previous,
                        width,
                        height,
                    });
                    return;
                }
                DeleteObject(bitmap);
            }
            DeleteDC(memory);
        }
    }
}
impl Drop for PaintBuffer {
    fn drop(&mut self) {
        // SAFETY: both handles belong to this buffer; restore the stock bitmap first.
        unsafe {
            SelectObject(self.dc, self.previous);
            DeleteObject(self.bitmap);
            DeleteDC(self.dc);
        }
    }
}

struct PatternBrush(HBRUSH);
impl Drop for PatternBrush {
    fn drop(&mut self) {
        // SAFETY: only our WM_CTLCOLOR handler lends this brush to Win32.
        unsafe { DeleteObject(self.0) };
    }
}

fn bitmap_info(width: i32, height: i32) -> BITMAPINFO {
    BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..Default::default()
        },
        ..Default::default()
    }
}

struct Window(HWND);
impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: run owns this HWND and drops the guard before its callback state.
        unsafe {
            if IsWindow(self.0) != 0 {
                DestroyWindow(self.0);
            }
        }
    }
}
struct App {
    metadata: ncat_presentation::metadata::MetadataOverlay,
    metadata_hwnd: HWND,
    metadata_copy: HWND,
    metadata_bounds: Cell<Option<(i32, i32, i32, i32)>>,
    hwnd: HWND,
    startup_image: bool,
    normal_size: Option<WindowSize>,
    controls: Vec<(u16, HWND, i32, i32)>,
    paint_buffer: Option<PaintBuffer>,
    bar_tint: Option<PaintBuffer>,
    glass_sample: Option<PaintBuffer>,
    metadata_glass: Option<PaintBuffer>,
    metadata_brush: Option<PatternBrush>,
    glass_pending: bool,
    bars: ReaderBars,
    visible_bars: [bool; 2],
    visible_animation: bool,
    visible_info: bool,
    bar_layout_size: Option<(i32, i32)>,
    in_size_move: bool,
    live_sizing: bool,
    tracking_mouse: HWND,
    backend: Backend,
    events: mpsc::Receiver<Prepared>,
    stop: Arc<AtomicBool>,
    book: Option<Arc<Book>>,
    frame: Option<Display>,
    selected: usize,
    page_loading: bool,
    status: String,
    last_title: RefCell<String>,
    reader: ReaderView,
    playback: Playback,
    animation_view: View,
    rtl: bool,
    controls_input: bool,
    fullscreen: Option<(isize, RECT)>,
    closing: Option<Instant>,
    drag: Option<(i32, i32)>,
    pan: (i32, i32),
    wheel: f64,
    last_wheel: Option<Instant>,
    smoke: Smoke,
    smoke_frames: u32,
    navigation_left: u32,
    modal_load_generation: Option<u64>,
    success: bool,
    resized_viewport: Option<(i32, i32)>,
    resize_test_phase: u8,
    resize_test_expected: Option<u32>,
    paint_count: u32,
    buffered_picture: Option<(i32, i32)>,
    glass_draw_count: u32,
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

unsafe fn work_area(hwnd: HWND) -> Option<RECT> {
    // SAFETY: queried on the UI thread for its own window.
    unsafe {
        monitor_info(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST)).map(|info| info.rcWork)
    }
}

unsafe fn monitor_info(monitor: HMONITOR) -> Option<MONITORINFO> {
    // SAFETY: the initialized monitor info is local to the call.
    unsafe {
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        (GetMonitorInfoW(monitor, &mut info) != 0).then_some(info)
    }
}

fn dpi_units(value: i32, from_dpi: u32, to_dpi: u32) -> i32 {
    let from = i64::from(from_dpi.max(1));
    ((i64::from(value.max(1)) * i64::from(to_dpi) + from / 2) / from).clamp(1, i64::from(i32::MAX))
        as i32
}

unsafe fn window_dpi(hwnd: HWND) -> u32 {
    // SAFETY: a live HWND on its owning thread; zero means unavailable.
    match unsafe { GetDpiForWindow(hwnd) } {
        0 => BASE_DPI,
        dpi => dpi,
    }
}

unsafe fn position_in_work_area(hwnd: HWND, rect: RECT, work: RECT, width: i32, height: i32) {
    // SAFETY: called on the owning UI thread; SetWindowPos does not retain data.
    unsafe {
        SetWindowPos(
            hwnd,
            ptr::null_mut(),
            rect.left
                .clamp(work.left, (work.right - width).max(work.left)),
            rect.top
                .clamp(work.top, (work.bottom - height).max(work.top)),
            width,
            height,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

unsafe fn set_child_visible(hwnd: HWND, show: bool) {
    // SAFETY: caller owns the child on this thread. Check its own style, not
    // ancestor visibility (the parent can still be hidden during creation).
    unsafe {
        if (GetWindowLongPtrW(hwnd, GWL_STYLE) & WS_VISIBLE as isize != 0) != show {
            ShowWindow(hwnd, if show { SW_SHOWNA } else { SW_HIDE });
        }
    }
}

fn stage(event: Event) -> Prepared {
    let pixels = if let Event::Frame { frame, .. } = &event {
        let source: &[u8] = frame.as_ref().as_ref();
        let mut bgra = Vec::new();
        if bgra.try_reserve_exact(source.len()).is_err() {
            return Prepared {
                event: Event::Error {
                    generation: event.generation(),
                    code: ncat_protocol::ErrorCode::Limit,
                },
                pixels: None,
            };
        }
        for pixel in source.as_chunks::<4>().0 {
            let alpha = u32::from(pixel[3]);
            let channel =
                |value: u8| ((u32::from(value) * alpha + 32 * (255 - alpha) + 127) / 255) as u8;
            bgra.extend([channel(pixel[2]), channel(pixel[1]), channel(pixel[0]), 255]);
        }
        Some(bgra)
    } else {
        None
    };
    Prepared { event, pixels }
}

impl App {
    fn new(smoke: Smoke) -> io::Result<(Self, mpsc::SyncSender<Prepared>)> {
        let backend = Backend::with_worker_command(
            WorkerCommand::same_executable(std::env::current_exe()?),
            BackendOptions::from_env()?,
        )?;
        let (sender, events) = mpsc::sync_channel(2);
        Ok((
            Self {
                metadata: ncat_presentation::metadata::MetadataOverlay::default(),
                metadata_hwnd: ptr::null_mut(),
                metadata_copy: ptr::null_mut(),
                metadata_bounds: Cell::new(None),
                hwnd: ptr::null_mut(),
                startup_image: false,
                normal_size: None,
                controls: Vec::new(),
                paint_buffer: None,
                bar_tint: None,
                glass_sample: None,
                metadata_glass: None,
                metadata_brush: None,
                glass_pending: false,
                bars: ReaderBars::default(),
                visible_bars: [false; 2],
                visible_animation: false,
                visible_info: false,
                bar_layout_size: None,
                in_size_move: false,
                live_sizing: false,
                tracking_mouse: ptr::null_mut(),
                backend,
                events,
                stop: Arc::new(AtomicBool::new(false)),
                book: None,
                frame: None,
                selected: 0,
                page_loading: false,
                status: "우클릭: 열기".into(),
                last_title: RefCell::new(String::new()),
                reader: ReaderView::default(),
                playback: Playback::default(),
                animation_view: View::default(),
                rtl: false,
                controls_input: false,
                fullscreen: None,
                closing: None,
                drag: None,
                pan: (0, 0),
                wheel: 0.0,
                last_wheel: None,
                smoke,
                smoke_frames: 0,
                navigation_left: if smoke == Smoke::Navigation { 500 } else { 0 },
                modal_load_generation: None,
                success: false,
                resized_viewport: None,
                resize_test_phase: 0,
                resize_test_expected: None,
                paint_count: 0,
                buffered_picture: None,
                glass_draw_count: 0,
            },
            sender,
        ))
    }
    fn control(&self, id: u16) -> HWND {
        self.controls
            .iter()
            .find(|entry| entry.0 == id)
            .map_or(ptr::null_mut(), |entry| entry.1)
    }
    unsafe fn text(&self, id: u16, text: &str) {
        // SAFETY: controls belong to this UI thread, and the buffer lives through the call.
        unsafe {
            SetWindowTextW(self.control(id), wide(text).as_ptr());
        }
    }
    unsafe fn value(&self, id: u16) -> String {
        unsafe {
            let hwnd = self.control(id);
            let len = GetWindowTextLengthW(hwnd).clamp(0, 16384) as usize;
            let mut buffer = vec![0; len + 1];
            let read =
                GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32).max(0) as usize;
            String::from_utf16_lossy(&buffer[..read])
        }
    }
    unsafe fn viewport(&self) -> RECT {
        unsafe {
            let mut rect: RECT = std::mem::zeroed();
            GetClientRect(self.hwnd, &mut rect);
            rect
        }
    }
    fn view(&self) -> View {
        View {
            width: ncat_protocol::MAX_VIEW_DIMENSION,
            height: ncat_protocol::MAX_VIEW_DIMENSION,
            quarter_turns: self.reader.quarter_turns,
        }
    }

    unsafe fn remember_normal_size(&mut self) {
        if self.fullscreen.is_some() {
            return;
        }
        // SAFETY: placement is initialized for this UI-thread window.
        unsafe {
            let mut placement = WINDOWPLACEMENT {
                length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
                ..Default::default()
            };
            if GetWindowPlacement(self.hwnd, &mut placement) != 0 {
                let rect = placement.rcNormalPosition;
                let dpi = window_dpi(self.hwnd);
                self.normal_size = WindowSize::new(
                    dpi_units(rect.right - rect.left, dpi, BASE_DPI),
                    dpi_units(rect.bottom - rect.top, dpi, BASE_DPI),
                );
            }
        }
    }

    unsafe fn grow_for_startup_image(&mut self) {
        // SAFETY: hwnd and temporary rectangles belong to the viewer's UI thread.
        unsafe {
            if !std::mem::take(&mut self.startup_image)
                || self.fullscreen.is_some()
                || IsZoomed(self.hwnd) != 0
                || IsIconic(self.hwnd) != 0
            {
                return;
            }
            let viewport = self.viewport();
            if viewport.right <= 0 || viewport.bottom <= 0 {
                return;
            }
            let mut rect = RECT::default();
            let Some(work) =
                work_area(self.hwnd).filter(|_| GetWindowRect(self.hwnd, &mut rect) != 0)
            else {
                return;
            };
            let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
            let frame = (width - viewport.right, height - viewport.bottom);
            let image = self.picture_dimensions(viewport);
            let (dx, dy) = window_state::startup_growth(
                image,
                (viewport.right, viewport.bottom),
                (
                    work.right - work.left - frame.0,
                    work.bottom - work.top - frame.1,
                ),
            );
            if dx != 0 || dy != 0 {
                position_in_work_area(self.hwnd, rect, work, width + dx, height + dy);
            }
        }
    }
    fn bar_rect(bar: Bar, viewport: RECT) -> RECT {
        let ((width, height), top) = match bar {
            Bar::Top => (TOP_SIZE, 8),
            Bar::Bottom => (BOTTOM_SIZE, viewport.bottom - BOTTOM_SIZE.1 - 8),
        };
        let left = if bar == Bar::Top {
            ((viewport.right - width) / 2).max(0)
        } else {
            (viewport.right - width - 8).max(0)
        };
        RECT {
            left,
            right: left + width,
            top,
            bottom: top + height,
        }
    }

    fn zoom_readout_rect(viewport: RECT) -> RECT {
        let tools = Self::bar_rect(Bar::Bottom, viewport);
        RECT {
            left: (tools.left - ZOOM_READOUT_WIDTH).max(0),
            right: tools.left,
            ..tools
        }
    }

    fn animation_rect(viewport: RECT) -> RECT {
        RECT {
            left: 8,
            right: 8 + ANIMATION_SIZE.0,
            top: viewport.bottom - ANIMATION_SIZE.1 - 8,
            bottom: viewport.bottom - 8,
        }
    }

    fn control_rect(id: u16, viewport: RECT) -> RECT {
        if id == METADATA {
            Self::info_rect(viewport)
        } else if animation_control(id) {
            Self::animation_rect(viewport)
        } else {
            Self::bar_rect(control_bar(id), viewport)
        }
    }

    fn info_rect(viewport: RECT) -> RECT {
        let right = viewport.right - 8;
        RECT {
            left: (right - INFO_SIZE.0).max(0),
            right,
            top: 8,
            bottom: 8 + INFO_SIZE.1,
        }
    }

    unsafe fn layout(&mut self) {
        unsafe {
            let viewport = self.viewport();
            if viewport.right == 0 || viewport.bottom == 0 || IsIconic(self.hwnd) != 0 {
                self.bar_layout_size = None;
                return;
            }
            let size = (viewport.right, viewport.bottom);
            let moved = self.bar_layout_size != Some(size);
            self.bar_layout_size = Some(size);
            let focus = GetFocus();
            let editing = [PAGE, FRAME_NUMBER, SPEED]
                .into_iter()
                .find(|&id| !focus.is_null() && focus == self.control(id))
                .map(control_bar);
            let next = [Bar::Top, Bar::Bottom]
                .map(|bar| !self.live_sizing && self.bars.visible(bar, editing));
            let animation = next[Bar::Bottom as usize] && self.playback.active;
            let info = !self.live_sizing;
            let changed = next != self.visible_bars
                || animation != self.visible_animation
                || info != self.visible_info;
            let show = |id| {
                if id == METADATA {
                    info
                } else if animation_control(id) {
                    animation
                } else {
                    next[control_bar(id) as usize]
                }
            };
            if moved || changed {
                let mut batch = BeginDeferWindowPos(self.controls.len() as i32);
                for &(id, hwnd, x, y) in &self.controls {
                    if !batch.is_null() {
                        let rect = Self::control_rect(id, viewport);
                        batch = DeferWindowPos(
                            batch,
                            hwnd,
                            ptr::null_mut(),
                            rect.left + x,
                            rect.top + y,
                            0,
                            0,
                            Self::bar_position_flags(moved, changed, show(id)),
                        );
                    }
                }
                if batch.is_null() || EndDeferWindowPos(batch) == 0 {
                    for &(id, hwnd, x, y) in &self.controls {
                        let rect = Self::control_rect(id, viewport);
                        SetWindowPos(
                            hwnd,
                            ptr::null_mut(),
                            rect.left + x,
                            rect.top + y,
                            0,
                            0,
                            Self::bar_position_flags(moved, changed, show(id)),
                        );
                    }
                }
                self.visible_bars = next;
                self.visible_animation = animation;
                self.visible_info = info;
                self.metadata_layout();
                // Let USER32 invalidate vacated child rectangles as well as
                // repainting the composited backdrop and the controls.
                RedrawWindow(
                    self.hwnd,
                    ptr::null(),
                    ptr::null_mut(),
                    RDW_INVALIDATE | RDW_ALLCHILDREN | RDW_ERASE,
                );
            }
        }
    }

    fn bar_position_flags(moved: bool, changed: bool, visible: bool) -> u32 {
        SWP_NOSIZE
            | SWP_NOZORDER
            | SWP_NOACTIVATE
            | SWP_NOCOPYBITS
            | if moved { 0 } else { SWP_NOMOVE }
            | if !changed {
                0
            } else if visible {
                SWP_SHOWWINDOW
            } else {
                SWP_HIDEWINDOW
            }
    }

    unsafe fn sync_pins(&mut self) {
        unsafe {
            for (bar, id) in [(Bar::Top, TOP_PIN), (Bar::Bottom, BOTTOM_PIN)] {
                self.text(
                    id,
                    if self.bars.pinned(bar) {
                        "고정 해제"
                    } else {
                        "고정"
                    },
                );
            }
            self.layout();
        }
    }

    unsafe fn track_bars(&mut self, target: HWND, leaving: bool) {
        unsafe {
            let previous = self.tracking_mouse;
            if leaving && self.tracking_mouse == target {
                self.tracking_mouse = ptr::null_mut();
            }
            if !leaving && self.tracking_mouse != target {
                let mut track = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: target,
                    dwHoverTime: 0,
                };
                if TrackMouseEvent(&mut track) != 0 {
                    self.tracking_mouse = target;
                }
            }
            if previous != self.tracking_mouse {
                for hwnd in [previous, self.tracking_mouse] {
                    if !hwnd.is_null() && hwnd != self.hwnd {
                        InvalidateRect(hwnd, ptr::null(), 0);
                    }
                }
            }
            let viewport = self.viewport();
            let mut point = POINT::default();
            let inside = GetCursorPos(&mut point) != 0
                && ScreenToClient(self.hwnd, &mut point) != 0
                && (0..viewport.right).contains(&point.x);
            let before = self.bars;
            self.bars.point(
                inside.then_some(point.y),
                viewport.bottom,
                14,
                TOP_SIZE.1 + 10,
                BOTTOM_SIZE.1 + 10,
            );
            if self.bars != before {
                self.layout();
            }
        }
    }
    unsafe fn metadata_layout(&self) {
        unsafe {
            if self.metadata_hwnd.is_null() {
                return;
            }
            let viewport = self.viewport();
            let width = (viewport.right - 24).min(480);
            let columns = ((width - 24) / 8).max(1) as usize;
            let lines: usize = self
                .metadata
                .text
                .lines()
                .map(|line| line.chars().count().div_ceil(columns).max(1))
                .sum();
            let top = Self::info_rect(viewport).bottom + 8 + METADATA_TOOLBAR_HEIGHT;
            let bottom = BOTTOM_SIZE.1 + 12;
            let height = (viewport.bottom - top - bottom)
                .min(300 - METADATA_TOOLBAR_HEIGHT)
                .min((lines * 20 + 20) as i32);
            let show = self.metadata.visible() && width >= 80 && height >= 40;
            let bounds = (viewport.right - width - 12, top, width, height);
            if show && self.metadata_bounds.get() != Some(bounds) {
                self.metadata_bounds.set(Some(bounds));
                SetWindowPos(
                    self.metadata_hwnd,
                    ptr::null_mut(),
                    bounds.0,
                    bounds.1,
                    width,
                    height,
                    SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOCOPYBITS,
                );
                SetWindowPos(
                    self.metadata_copy,
                    ptr::null_mut(),
                    bounds.0 + width - 40,
                    top - 32,
                    32,
                    28,
                    SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOCOPYBITS,
                );
            }
            set_child_visible(self.metadata_hwnd, show);
            set_child_visible(self.metadata_copy, show);
        }
    }
    unsafe fn sync_metadata(&self) {
        unsafe {
            if !self.metadata_hwnd.is_null() {
                SetWindowTextW(
                    self.metadata_hwnd,
                    wide(&self.metadata.text.replace('\n', "\r\n")).as_ptr(),
                );
            }
            self.metadata_layout();
            self.invalidate_picture();
        }
    }

    unsafe fn toggle_metadata(&mut self) {
        self.metadata.toggle();
        unsafe {
            self.text(
                METADATA,
                if self.metadata.enabled {
                    "메타데이터 닫기"
                } else {
                    "메타데이터 열기"
                },
            );
            self.sync_metadata();
        }
    }
    unsafe fn metadata_test(&mut self) {
        unsafe {
            KillTimer(self.hwnd, METADATA_TEST);
            let result = (|| {
                let visible = |hwnd| IsWindowVisible(hwnd) != 0;
                if self.metadata.enabled
                    || visible(self.metadata_hwnd)
                    || visible(self.metadata_copy)
                {
                    return false;
                }
                if self.smoke == Smoke::MetadataWindow {
                    // UI-only fixture for checks that cannot start a protected worker.
                    self.metadata.update(&[ncat_protocol::MetadataEntry {
                        name: "Title".into(),
                        value: "<literal> 한글 synthetic metadata".into(),
                    }]);
                } else if !self.metadata.text.contains("<literal>") {
                    return false;
                }
                self.sync_metadata();
                if IsWindowVisible(self.metadata_hwnd) != 0 {
                    return false;
                }
                let focus = GetFocus();
                if !self.key(VK_TAB as u32)
                    || !visible(self.metadata_hwnd)
                    || !visible(self.metadata_copy)
                {
                    return false;
                }
                if GetFocus() != focus
                    || self
                        .metadata_bounds
                        .get()
                        .is_none_or(|(left, top, width, _)| {
                            top < Self::info_rect(self.viewport()).bottom
                                || left + width + 12 != self.viewport().right
                        })
                {
                    return false;
                }
                self.live_sizing = true;
                self.layout();
                self.metadata_layout();
                let kept_during_resize = visible(self.metadata_hwnd) && visible(self.metadata_copy);
                self.live_sizing = false;
                self.layout();
                if !kept_during_resize {
                    return false;
                }
                self.command(METADATA, 0);
                if visible(self.metadata_hwnd) || visible(self.metadata_copy) {
                    return false;
                }
                self.key(VK_TAB as u32);
                self.metadata.clear();
                self.sync_metadata();
                !visible(self.metadata_hwnd) && !visible(self.metadata_copy)
            })();
            self.success = result;
            if result {
                println!(
                    "metadata_window_smoke=ok synthetic={} default_off=true tab=true focus_unchanged=true",
                    self.smoke == Smoke::MetadataWindow,
                );
            }
            self.close();
        }
    }
    unsafe fn playback_ui(&mut self) {
        unsafe {
            let p = self.playback;
            for id in [FRAME_PREVIOUS, FRAME_NEXT, PLAY, SPEED] {
                EnableWindow(self.control(id), i32::from(p.active));
            }
            for id in [FRAME_NUMBER, FRAME_GO] {
                EnableWindow(self.control(id), i32::from(p.can_jump()));
            }
            self.text(
                PLAY,
                if p.paused || p.ended {
                    "재생"
                } else {
                    "일시정지"
                },
            );
            self.update_title();
            self.layout();
        }
    }
    unsafe fn queue_resize(&self) {
        unsafe {
            InvalidateRect(self.hwnd, ptr::null(), 0);
            // SetWindowPos/ShowWindow can synchronously reenter procedure while
            // App is borrowed. Defer state changes until that borrow has ended.
            PostMessageW(self.hwnd, VIEWPORT_CHANGED, 0, 0);
        }
    }
    unsafe fn invalidate_picture(&self) {
        unsafe {
            InvalidateRect(self.hwnd, &self.viewport(), 0);
        }
    }
    unsafe fn viewport_changed(&mut self) {
        unsafe {
            self.layout();
            self.metadata_layout();
            let rect = self.viewport();
            if IsIconic(self.hwnd) == 0 && rect.right > 0 && rect.bottom > 0 {
                self.resized_viewport = Some((rect.right, rect.bottom));
            }
        }
    }

    unsafe fn refresh_glass(&self) {
        unsafe {
            InvalidateRect(self.hwnd, ptr::null(), 0);
            if IsWindowVisible(self.metadata_hwnd) != 0 {
                InvalidateRect(self.metadata_hwnd, ptr::null(), 1);
            }
        }
    }
    unsafe fn resize_test(&mut self) {
        unsafe {
            if let Some(paints) = self.resize_test_expected.take() {
                // The window manager may adjust maximize bounds asynchronously.
                // Validate the settled client area, not the immediate ShowWindow result.
                let viewport = self.viewport();
                let expected = Some((viewport.right, viewport.bottom));
                if self.resized_viewport != expected
                    || self.paint_count <= paints
                    || !self.bar_geometry_matches()
                    || (self.smoke == Smoke::ResizeFrame
                        && self.buffered_picture != Some(self.picture_dimensions(self.viewport())))
                {
                    eprintln!(
                        "nCatImage: resize phase={} expected={expected:?} actual={:?} paints={paints}->{} buffered={:?}",
                        self.resize_test_phase,
                        self.resized_viewport,
                        self.paint_count,
                        self.buffered_picture,
                    );
                    self.close();
                    return;
                }
            }
            match self.resize_test_phase {
                0 => {
                    if self.smoke == Smoke::Resize
                        && address_dialog(self.hwnd, true).as_deref()
                            != Some("smb://server/share/book.cbz")
                    {
                        eprintln!("nCatImage: address dialog did not return its edit value");
                        self.close();
                        return;
                    }
                    SetWindowPos(
                        self.hwnd,
                        ptr::null_mut(),
                        0,
                        0,
                        720,
                        540,
                        SWP_NOMOVE | SWP_NOZORDER,
                    );
                }
                1 => {
                    ShowWindow(self.hwnd, SW_MAXIMIZE);
                }
                2 => {
                    ShowWindow(self.hwnd, SW_RESTORE);
                }
                3 | 4 => self.fullscreen(),
                5 | 6 => {
                    let view = self.view();
                    let generation = self.backend.generation();
                    if self.resize_test_phase == 5 {
                        self.bars.toggle_both();
                        self.bars.point(None, 0, 0, 0, 0);
                    } else {
                        self.bars.toggle_pin(Bar::Top);
                        let height = self.viewport().bottom;
                        self.bars
                            .point(Some(height - 2), height, 14, TOP_SIZE.1, BOTTOM_SIZE.1);
                    }
                    self.sync_pins();
                    let expected = if self.resize_test_phase == 5 {
                        [false, false]
                    } else {
                        [true, true]
                    };
                    if self.visible_bars != expected
                        || self.view() != view
                        || self.backend.generation() != generation
                    {
                        eprintln!("nCatImage: overlay visibility changed the image view or decode");
                        self.close();
                        return;
                    }
                }
                7 => {
                    ShowWindow(self.hwnd, SW_MINIMIZE);
                }
                8 => {
                    ShowWindow(self.hwnd, SW_RESTORE);
                }
                9 => {
                    // Exercise WM_SIZE with App available, like a real drag.
                    PostMessageW(self.hwnd, LIVE_RESIZE_TEST, 0, 0);
                }
                10 | 11 if self.smoke == Smoke::ResizeFrame => {
                    let generation = self.backend.generation();
                    if self.resize_test_phase == 10 {
                        for _ in 0..4 {
                            self.zoom(1.25);
                        }
                    } else {
                        self.zoom(0.0);
                    }
                    if self.frame.is_none() || self.backend.generation() != generation {
                        eprintln!("nCatImage: zoom must retain the frame without decoding");
                        self.close();
                        return;
                    }
                }
                _ => {
                    if self.smoke == Smoke::ResizeFrame && self.glass_draw_count == 0 {
                        eprintln!("nCatImage: no panel rendered the softened image");
                        self.close();
                        return;
                    }
                    self.success = true;
                    self.close();
                    return;
                }
            }
            self.resize_test_phase += 1;
            if IsIconic(self.hwnd) == 0 {
                self.resize_test_expected = Some(self.paint_count);
            }
        }
    }
    unsafe fn bar_geometry_matches(&self) -> bool {
        unsafe {
            let viewport = self.viewport();
            let mut origin = POINT::default();
            if ClientToScreen(self.hwnd, &mut origin) == 0 {
                return false;
            }
            self.controls.iter().all(|&(id, hwnd, x, y)| {
                let expected = Self::control_rect(id, viewport);
                let expected_visible = if id == METADATA {
                    self.visible_info
                } else if animation_control(id) {
                    self.visible_animation
                } else {
                    self.visible_bars[control_bar(id) as usize]
                };
                let mut actual = RECT::default();
                let visible = IsWindowVisible(hwnd) != 0;
                let valid = GetWindowRect(hwnd, &mut actual) != 0
                    && actual.left - origin.x == expected.left + x
                    && actual.top - origin.y == expected.top + y
                    && visible == expected_visible;
                if !valid {
                    eprintln!(
                        "bar control {id}: actual=({}, {}, {visible}) expected=({}, {}, {})",
                        actual.left - origin.x,
                        actual.top - origin.y,
                        expected.left + x,
                        expected.top + y,
                        expected_visible
                    );
                }
                valid
            })
        }
    }
    unsafe fn navigation_ui(&self) {
        unsafe {
            let len = self
                .book
                .as_ref()
                .filter(|book| book.complete)
                .map_or(0, |book| book.pages.len());
            self.text(PAGE, &(self.selected + 1).to_string());
            EnableWindow(
                self.control(PREVIOUS),
                i32::from(len > 0 && self.selected > 0),
            );
            EnableWindow(
                self.control(NEXT),
                i32::from(len > 0 && self.selected + 1 < len),
            );
            for id in [PAGE, PAGE_GO] {
                EnableWindow(self.control(id), i32::from(len > 0));
            }
            self.update_title();
        }
    }

    unsafe fn set_status(&mut self, text: &str) {
        self.status = text.into();
        unsafe { self.update_title() };
    }

    unsafe fn update_title(&self) {
        let page = self.book.as_ref().map(|book| {
            let name = book
                .pages
                .get(self.selected)
                .map_or("", |page| page.name.as_str());
            (
                self.selected + 1,
                book.complete.then_some(book.pages.len()),
                name,
            )
        });
        let title = ncat_presentation::reader_title(
            concat!("nCatImage v", env!("CARGO_PKG_VERSION")),
            page,
            self.playback,
            &self.status,
        );
        if *self.last_title.borrow() != title {
            unsafe { SetWindowTextW(self.hwnd, wide(&title).as_ptr()) };
            self.last_title.replace(title);
        }
    }
    unsafe fn reset(&mut self, autoplay: bool) {
        unsafe {
            KillTimer(self.hwnd, ANIMATION);
            self.playback = Playback {
                speed: self.playback.speed,
                paused: !autoplay && self.playback.paused,
                ..Playback::default()
            };
            self.playback_ui();
        }
    }
    unsafe fn open(&mut self, path: PathBuf) {
        self.startup_image = false;
        unsafe { self.open_impl(path) };
    }

    unsafe fn open_impl(&mut self, path: PathBuf) {
        unsafe {
            self.reset(true);
            self.page_loading = false;
            self.book = None;
            self.frame = None;
            self.metadata.clear();
            self.sync_metadata();
            self.selected = 0;
            self.reader = ReaderView::default();
            self.pan = (0, 0);
            self.animation_view = self.view();
            if let Err(code) = self.backend.open(path, self.animation_view) {
                self.startup_image = false;
                self.set_status(&code.to_string());
            } else {
                self.page_loading = true;
                self.set_status("읽는 중…");
            }
            self.navigation_ui();
            self.invalidate_picture();
            self.controls_input = false;
            SetFocus(self.hwnd);
        }
    }
    unsafe fn load(&mut self, index: usize) {
        unsafe {
            if self.page_loading {
                return;
            }
            let Some(book) = self
                .book
                .clone()
                .filter(|b| b.complete && index < b.pages.len())
            else {
                return;
            };
            self.reset(index != self.selected);
            self.selected = index;
            self.animation_view = self.view();
            match self.backend.load(book, index, self.animation_view) {
                Ok(_) => {
                    self.page_loading = true;
                    self.set_status("읽는 중…");
                }
                Err(code) => self.set_status(&code.to_string()),
            }
            self.navigation_ui();
        }
    }
    unsafe fn navigate(&mut self, forward: bool) {
        unsafe {
            if self.page_loading {
                return;
            }
            let len = self
                .book
                .as_ref()
                .filter(|b| b.complete)
                .map_or(0, |b| b.pages.len());
            if let Some(index) = adjacent(self.selected, len, forward)
                && index != self.selected
            {
                self.reader.zoom = 1.0;
                self.reader.fit = false;
                self.reader.quarter_turns = 0;
                self.pan = (0, 0);
                self.load(index);
            }
        }
    }
    unsafe fn animation(&mut self, seek: Option<u32>) {
        unsafe {
            if !self.playback.active || self.playback.busy {
                return;
            }
            let Some(book) = self.book.clone() else {
                return;
            };
            KillTimer(self.hwnd, ANIMATION);
            let result = if let Some(index) = seek {
                self.backend
                    .animation_seek(book, self.selected, self.animation_view, index)
            } else {
                self.backend
                    .animation_next(book, self.selected, self.animation_view)
            };
            self.playback.busy = result.is_ok();
            self.playback_ui();
        }
    }
    unsafe fn schedule(&self) {
        unsafe {
            KillTimer(self.hwnd, ANIMATION);
            if self.playback.active
                && !self.playback.paused
                && !self.playback.ended
                && !self.playback.busy
            {
                SetTimer(self.hwnd, ANIMATION, self.playback.delay() as u32, None);
            }
        }
    }
    unsafe fn step(&mut self, forward: bool) {
        unsafe {
            if !self.playback.active {
                return;
            }
            self.playback.paused = true;
            KillTimer(self.hwnd, ANIMATION);
            self.playback_ui();
            if self.playback.busy {
                return;
            }
            if forward && !self.playback.ended {
                self.animation(None);
            } else if !forward && self.playback.index > 0 {
                self.animation(Some(self.playback.index - 1));
            }
        }
    }
    unsafe fn play(&mut self) {
        unsafe {
            if !self.playback.active {
                return;
            }
            self.playback.paused = !self.playback.paused;
            self.playback_ui();
            if !self.playback.paused && self.playback.ended {
                self.animation(Some(0));
            } else {
                self.schedule();
            }
        }
    }
    unsafe fn zoom(&mut self, factor: f64) {
        unsafe {
            if self.book.is_none() {
                return;
            }
            let previous = self.reader;
            let viewport = self.viewport();
            self.reader
                .change_zoom(factor, (viewport.right, viewport.bottom), 1.0);
            if self.reader == previous && self.pan == (0, 0) {
                return;
            }
            self.pan = (0, 0);
            self.glass_pending = true;
            SetTimer(self.hwnd, GLASS_SETTLE, 120, None);
            self.invalidate_picture();
        }
    }
    unsafe fn fullscreen(&mut self) {
        unsafe {
            if let Some((style, rect)) = self.fullscreen.take() {
                SetWindowLongPtrW(self.hwnd, GWL_STYLE, style);
                SetWindowPos(
                    self.hwnd,
                    ptr::null_mut(),
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    SWP_NOZORDER | SWP_FRAMECHANGED,
                );
            } else {
                self.remember_normal_size();
                let style = GetWindowLongPtrW(self.hwnd, GWL_STYLE);
                let mut rect: RECT = std::mem::zeroed();
                GetWindowRect(self.hwnd, &mut rect);
                let mut monitor: MONITORINFO = std::mem::zeroed();
                monitor.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
                if GetMonitorInfoW(
                    MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST),
                    &mut monitor,
                ) == 0
                {
                    return;
                }
                self.fullscreen = Some((style, rect));
                SetWindowLongPtrW(
                    self.hwnd,
                    GWL_STYLE,
                    style & !(WS_OVERLAPPEDWINDOW as isize),
                );
                let r = monitor.rcMonitor;
                SetWindowPos(
                    self.hwnd,
                    ptr::null_mut(),
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOZORDER | SWP_FRAMECHANGED,
                );
            }
            self.controls_input = false;
            SetFocus(self.hwnd);
            self.layout();
            self.queue_resize();
        }
    }
    unsafe fn consume(&mut self, prepared: Prepared) {
        unsafe {
            if self.closing.is_some() || !self.backend.accepts(&prepared.event) {
                return;
            }
            let generation = prepared.event.generation();
            match prepared.event {
                Event::Neighbor { result, .. } => match result {
                    Ok(Some(path)) => self.open(path),
                    Ok(None) => self.set_status("이동할 폴더/압축 파일 없음"),
                    Err(code) => self.set_status(&code.to_string()),
                },
                Event::Catalog { book, .. } => {
                    self.selected = book.selected;
                    self.book = Some(book);
                    self.navigation_ui();
                    self.viewport_changed();
                }
                Event::Frame { frame, .. } => {
                    let Some(pixels) = prepared.pixels else {
                        return;
                    };
                    if self.metadata.update(&frame.info.metadata) {
                        self.sync_metadata();
                    }
                    self.reader.original = (frame.info.original_width, frame.info.original_height);
                    if let Some(animation) = &frame.info.animation {
                        self.playback.displayed(
                            animation.index,
                            animation.cycle,
                            animation.delay_ms,
                        );
                        self.playback.total = animation.total.or(self.playback.total);
                        self.text(FRAME_NUMBER, &(animation.index + 1).to_string());
                    } else {
                        self.reset(true);
                    }
                    self.set_status(&format!(
                        "{} · {}×{}",
                        frame.info.format, frame.info.original_width, frame.info.original_height,
                    ));
                    let animated = frame.info.animation.is_some();
                    let first_frame = self.frame.is_none();
                    self.frame = Some(Display {
                        frame,
                        bgra: pixels,
                    });
                    self.page_loading = false;
                    self.grow_for_startup_image();
                    self.refresh_glass();
                    self.playback_ui();
                    if matches!(self.smoke, Smoke::Frame | Smoke::Animation)
                        && (self.visible_animation != animated
                            || (IsWindowVisible(self.control(PLAY)) != 0) != animated
                            || self.last_title.borrow().contains("프레임 ") != animated)
                    {
                        eprintln!("nCatImage: animation panel or title does not match the image");
                        self.close();
                        return;
                    }
                    self.schedule();
                    InvalidateRect(self.hwnd, ptr::null(), 0);
                    self.smoke_frames += 1;
                    if self.smoke == Smoke::ResizeFrame && first_frame {
                        SetTimer(self.hwnd, RESIZE_TEST, 250, None);
                    }
                    if self.smoke == Smoke::MetadataFrame && first_frame {
                        self.metadata_test();
                    }
                    if self.modal_load_generation == Some(generation) {
                        self.success = self.reader.original == (1200, 800);
                        self.close();
                    }
                    if self.smoke == Smoke::Frame
                        || (self.smoke == Smoke::Animation && animated && self.smoke_frames >= 6)
                    {
                        self.success = true;
                        self.close();
                    }
                }
                Event::AnimationEnded { .. } => {
                    self.playback.finish();
                    self.playback_ui();
                    KillTimer(self.hwnd, ANIMATION);
                    if self.smoke == Smoke::Animation {
                        self.success = self.smoke_frames >= 2;
                        self.close();
                    }
                }
                Event::Error { code, .. } => {
                    self.startup_image = false;
                    self.page_loading = false;
                    self.reset(true);
                    self.frame = None;
                    self.metadata.clear();
                    self.sync_metadata();
                    self.set_status(&code.to_string());
                    self.invalidate_picture();
                    if self.smoke != Smoke::None {
                        eprintln!("nCatImage: GUI smoke error={code:?}");
                        self.close();
                    }
                }
            }
            if self.smoke == Smoke::ModalLoad
                && self.modal_load_generation.is_none()
                && self.frame.is_some()
                && self.book.as_ref().is_some_and(|book| book.complete)
            {
                self.load(2);
                self.modal_load_generation = Some(self.backend.generation());
                let mut message = MSG::default();
                let deadline = Instant::now() + Duration::from_secs(5);
                while Instant::now() < deadline {
                    if PeekMessageW(&mut message, self.hwnd, RESULT, RESULT, PM_REMOVE) != 0 {
                        DispatchMessageW(&message);
                        eprintln!("nested pump dispatched page result while App was borrowed");
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                eprintln!("nested pump did not receive page result");
                self.close();
            }
            if self.smoke == Smoke::Navigation && self.frame.is_some() && !self.page_loading {
                let len = self
                    .book
                    .as_ref()
                    .filter(|b| b.complete)
                    .map_or(0, |b| b.pages.len());
                if len > 0 {
                    if self.navigation_left > 0 {
                        self.navigation_left -= 1;
                        let index = (self.selected + 1) % len;
                        self.load(index);
                        let generation = self.backend.generation();
                        self.navigate(true);
                        self.navigate(false);
                        if !self.page_loading
                            || self.selected != index
                            || self.backend.generation() != generation
                        {
                            eprintln!(
                                "nCatImage: navigation accepted input before the page loaded"
                            );
                            self.close();
                        }
                    } else {
                        self.success = true;
                        self.close();
                    }
                }
            }
        }
    }
    unsafe fn close(&mut self) {
        unsafe {
            if self.closing.is_some() {
                return;
            }
            self.closing = Some(Instant::now());
            self.remember_normal_size();
            KillTimer(self.hwnd, RESIZE_TEST);
            KillTimer(self.hwnd, METADATA_TEST);
            self.stop.store(true, Ordering::Release);
            KillTimer(self.hwnd, ANIMATION);
            self.backend.shutdown();
            SetTimer(self.hwnd, SHUTDOWN, 10, None);
        }
    }
    unsafe fn command(&mut self, id: u16, notification: u16) {
        unsafe {
            if notification == EN_CHANGE as u16 {
                return;
            }
            match id {
                OPEN => {
                    if let Some(path) = file_dialog(self.hwnd) {
                        self.open(path);
                    }
                }
                FOLDER => {
                    if let Some(path) = folder_dialog(self.hwnd) {
                        self.open(path);
                    }
                }
                CONNECT => {
                    if let Some(address) = address_dialog(self.hwnd, false) {
                        match crate::address::parse(&address) {
                            Ok(path) => self.open(path),
                            Err(text) => self.set_status(text),
                        }
                    }
                }
                VIEW_LOG => log_dialog(self.hwnd, &self.backend.diagnostic_log()),
                COPY_METADATA if self.metadata.visible() => {
                    if !copy_metadata_to_clipboard(self.hwnd, &self.metadata.text) {
                        self.set_status("메타데이터를 복사하지 못했습니다");
                    }
                }
                PREVIOUS => self.navigate(false),
                NEXT => self.navigate(true),
                PAGE_GO => {
                    if let Ok(number) = self.value(PAGE).trim().parse::<usize>()
                        && number > 0
                    {
                        self.load(number - 1);
                    }
                }
                DIRECTION => {
                    self.rtl = !self.rtl;
                    self.text(DIRECTION, if self.rtl { "우→좌" } else { "좌→우" });
                }
                TOP_PIN | BOTTOM_PIN => {
                    self.bars.toggle_pin(control_bar(id));
                    self.sync_pins();
                }
                METADATA => self.toggle_metadata(),
                SMALLER => self.zoom(1.0 / 1.25),
                FIT => self.zoom(0.0),
                LARGER => self.zoom(1.25),
                ROTATE => {
                    if !self.page_loading {
                        self.reader.rotate(false);
                        self.load(self.selected);
                    }
                }
                FULLSCREEN => self.fullscreen(),
                FRAME_PREVIOUS => self.step(false),
                FRAME_NEXT => self.step(true),
                PLAY => self.play(),
                SPEED if notification == CBN_SELCHANGE as u16 => {
                    let index = SendMessageW(self.control(SPEED), CB_GETCURSEL, 0, 0) as usize;
                    if let Some(speed) = SPEEDS.get(index) {
                        self.playback.set_speed(*speed);
                        self.schedule();
                    }
                }
                FRAME_GO if self.playback.can_jump() => {
                    if let Ok(number) = self.value(FRAME_NUMBER).trim().parse::<u32>()
                        && (1..=ncat_protocol::MAX_ANIMATION_FRAMES).contains(&number)
                    {
                        self.animation(Some(number - 1));
                    }
                }
                _ => {}
            }
        }
    }
    unsafe fn key(&mut self, key: u32) -> bool {
        unsafe {
            let modifiers = Modifiers {
                primary: GetKeyState(VK_CONTROL as i32) < 0,
                shift: GetKeyState(VK_SHIFT as i32) < 0,
                alternate: GetKeyState(VK_MENU as i32) < 0,
                system: GetKeyState(VK_LWIN as i32) < 0 || GetKeyState(VK_RWIN as i32) < 0,
            };
            if key == VK_RETURN as u32 {
                let focus = GetFocus();
                for (id, action) in [(PAGE, PAGE_GO), (FRAME_NUMBER, FRAME_GO)] {
                    if focus == self.control(id) {
                        self.command(action, 0);
                        return true;
                    }
                }
            }
            let key = match key as u16 {
                VK_OEM_COMMA => Key::FramePrevious,
                VK_OEM_PERIOD => Key::FrameNext,
                VK_OEM_4 => Key::BookPrevious,
                VK_OEM_6 => Key::BookNext,
                VK_OEM_2 => Key::Playback,
                VK_F11 => Key::Fullscreen,
                VK_ESCAPE => Key::Escape,
                VK_OEM_3 => Key::ToggleInterface,
                VK_TAB => Key::Tab,
                VK_DOWN | VK_NEXT | VK_SPACE => Key::Next,
                VK_UP | VK_PRIOR | VK_BACK => Key::Previous,
                VK_LEFT => Key::Left,
                VK_RIGHT => Key::Right,
                VK_HOME => Key::First,
                VK_END => Key::Last,
                VK_OEM_PLUS | VK_ADD => Key::ZoomIn,
                VK_OEM_MINUS | VK_SUBTRACT => Key::ZoomOut,
                VK_F5 => Key::Reload,
                0x4f => Key::Open,
                0x4c => Key::Network,
                0x51 => Key::Quit,
                0x52 => Key::Rotate,
                0x44 => Key::Direction,
                0x39 => Key::Fit,
                _ => Key::Other,
            };
            match input::command(
                key,
                modifiers,
                self.controls_input && GetFocus() != self.hwnd,
            ) {
                Command::Native => return false,
                Command::Block => {}
                Command::ToggleMetadata => {
                    self.toggle_metadata();
                }
                Command::Open { folder } => self.command(if folder { FOLDER } else { OPEN }, 0),
                Command::Network => {
                    self.command(CONNECT, 0);
                }
                Command::Quit => self.close(),
                Command::Fullscreen => self.fullscreen(),
                Command::ToggleInterface => {
                    SetFocus(self.hwnd);
                    self.controls_input = false;
                    self.bars.toggle_both();
                    self.sync_pins();
                }
                Command::Page(key) => self.navigate(ncat_presentation::page_step(
                    key,
                    if self.rtl {
                        ncat_core::Direction::RightToLeft
                    } else {
                        ncat_core::Direction::LeftToRight
                    },
                )),
                Command::Book { forward } => {
                    if let Some(book) = self.book.clone() {
                        self.backend.find_neighbor(book, forward);
                    }
                }
                Command::First => self.load(0),
                Command::Last => {
                    if let Some(book) = &self.book {
                        self.load(book.pages.len().saturating_sub(1));
                    }
                }
                Command::ZoomIn => self.zoom(1.25),
                Command::ZoomOut => self.zoom(1.0 / 1.25),
                Command::Fit => self.zoom(0.0),
                Command::Rotate { backwards } => {
                    if !self.page_loading {
                        self.reader.rotate(backwards);
                        self.load(self.selected);
                    }
                }
                Command::Direction => self.command(DIRECTION, 0),
                Command::Reload => {
                    if let Some(book) = &self.book {
                        self.open(book.source.path());
                    }
                }
                Command::FramePrevious => self.step(false),
                Command::FrameNext => self.step(true),
                Command::Playback => self.play(),
            }
            true
        }
    }
    fn picture_dimensions(&self, rect: RECT) -> (i32, i32) {
        // A per-monitor-aware HWND exposes client and GDI coordinates in
        // device pixels. Dividing by monitor DPI here would shrink the image.
        self.reader
            .picture_size((rect.right, rect.bottom - rect.top), 1.0)
    }

    unsafe fn draw_picture(&mut self, dc: HDC, rect: RECT) -> Option<(i32, i32)> {
        let (w, h) = self.picture_dimensions(rect);
        let height = rect.bottom - rect.top;
        let mut drawn = None;
        // SAFETY: dc is a live paint or memory DC, clipped to the picture area.
        // Display owns the validated, tightly packed top-down BGRA pixels.
        unsafe {
            let brush = GetStockObject(DC_BRUSH);
            SetDCBrushColor(dc, 0x202020);
            if let Some(display) = &self.frame {
                let (x, y) = self
                    .reader
                    .picture_origin((w, h), (rect.right, height), self.pan);
                self.pan = (x.min(0), y.min(0));
                let y = rect.top + y;
                let width = display.frame.info.width as i32;
                let height = display.frame.info.height as i32;
                SetStretchBltMode(dc, HALFTONE);
                SetBrushOrgEx(dc, 0, 0, ptr::null_mut());
                let lines = StretchDIBits(
                    dc,
                    x,
                    y,
                    w,
                    h,
                    0,
                    0,
                    width,
                    height,
                    display.bgra.as_ptr().cast(),
                    &bitmap_info(width, height),
                    DIB_RGB_COLORS,
                    SRCCOPY,
                );
                if lines > 0 {
                    // The fallback draws directly to the window. Fill only the
                    // gutters so it never erases the image before replacing it.
                    let left = x.clamp(0, rect.right);
                    let right = (x + w).clamp(0, rect.right);
                    let top = y.clamp(rect.top, rect.bottom);
                    let bottom = (y + h).clamp(rect.top, rect.bottom);
                    for (left, top, right, bottom) in [
                        (0, rect.top, rect.right, top),
                        (0, bottom, rect.right, rect.bottom),
                        (0, top, left, bottom),
                        (right, top, rect.right, bottom),
                    ] {
                        if left < right && top < bottom {
                            FillRect(
                                dc,
                                &RECT {
                                    left,
                                    top,
                                    right,
                                    bottom,
                                },
                                brush,
                            );
                        }
                    }
                    drawn = Some((w, h));
                }
            }
            if drawn.is_none() {
                FillRect(dc, &rect, brush);
            }
        }
        drawn
    }

    unsafe fn paint(&mut self) {
        unsafe {
            let mut paint: PAINTSTRUCT = std::mem::zeroed();
            let dc = BeginPaint(self.hwnd, &mut paint);
            if !dc.is_null() {
                self.paint_count = self.paint_count.saturating_add(1);
                let rect = self.viewport();
                let mut dirty = RECT::default();
                if IntersectRect(&mut dirty, &rect, &paint.rcPaint) != 0 {
                    PaintBuffer::ensure(&mut self.paint_buffer, dc, rect.right, rect.bottom);
                    self.buffered_picture = None;
                    if let Some(memory) = self.paint_buffer.as_ref().map(|b| b.dc) {
                        IntersectClipRect(memory, dirty.left, dirty.top, dirty.right, dirty.bottom);
                        let drawn = self.draw_picture(memory, rect);
                        self.draw_bars(memory, rect);
                        SelectClipRgn(memory, ptr::null_mut());
                        if BitBlt(
                            dc,
                            dirty.left,
                            dirty.top,
                            dirty.right - dirty.left,
                            dirty.bottom - dirty.top,
                            memory,
                            dirty.left,
                            dirty.top,
                            SRCCOPY,
                        ) != 0
                        {
                            self.buffered_picture = drawn;
                        } else {
                            IntersectClipRect(dc, rect.left, rect.top, rect.right, rect.bottom);
                            self.draw_picture(dc, rect);
                            self.draw_bars(dc, rect);
                        }
                    } else {
                        IntersectClipRect(dc, rect.left, rect.top, rect.right, rect.bottom);
                        self.draw_picture(dc, rect);
                        self.draw_bars(dc, rect);
                    }
                }
            }
            EndPaint(self.hwnd, &paint);
        }
    }

    unsafe fn draw_bars(&mut self, dc: HDC, viewport: RECT) {
        // SAFETY: this paint runs on the owning UI thread. Temporary regions
        // and the saved clip are released before returning to the caller.
        unsafe {
            if !self.visible_info && !self.visible_bars.iter().any(|&visible| visible) {
                return;
            }
            PaintBuffer::ensure(&mut self.bar_tint, dc, 1, 1);
            let brush = GetStockObject(DC_BRUSH);
            SetDCBrushColor(dc, 0x181818);
            if let Some(tint) = &self.bar_tint {
                SetDCBrushColor(tint.dc, 0x181818);
                FillRect(
                    tint.dc,
                    &RECT {
                        right: 1,
                        bottom: 1,
                        ..Default::default()
                    },
                    brush,
                );
            }
            let copy_bar =
                self.metadata_bounds
                    .get()
                    .map_or(RECT::default(), |(left, top, width, _)| RECT {
                        left: left + width - 48,
                        top: top - METADATA_TOOLBAR_HEIGHT,
                        right: left + width,
                        bottom: top,
                    });
            let tools = Self::bar_rect(Bar::Bottom, viewport);
            let readout = Self::zoom_readout_rect(viewport);
            let bottom = if self.frame.is_some() {
                RECT {
                    left: readout.left,
                    ..tools
                }
            } else {
                tools
            };
            for (r, visible) in [
                (
                    Self::bar_rect(Bar::Top, viewport),
                    self.visible_bars[Bar::Top as usize],
                ),
                (bottom, self.visible_bars[Bar::Bottom as usize]),
                (Self::animation_rect(viewport), self.visible_animation),
                (Self::info_rect(viewport), self.visible_info),
                (copy_bar, IsWindowVisible(self.metadata_copy) != 0),
            ] {
                if !visible {
                    continue;
                }
                let region = CreateRoundRectRgn(r.left, r.top, r.right, r.bottom, 20, 20);
                if region.is_null() {
                    continue;
                }
                let saved = SaveDC(dc);
                let blended = if saved != 0 {
                    let ok = if ExtSelectClipRgn(dc, region, RGN_AND) != RGN_ERROR {
                        self.draw_glass_sample(dc, viewport, r, r);
                        self.bar_tint.as_ref().is_some_and(|tint| {
                            GdiAlphaBlend(
                                dc,
                                r.left,
                                r.top,
                                r.right - r.left,
                                r.bottom - r.top,
                                tint.dc,
                                0,
                                0,
                                1,
                                1,
                                BLENDFUNCTION {
                                    BlendOp: AC_SRC_OVER as u8,
                                    BlendFlags: 0,
                                    SourceConstantAlpha: 198,
                                    AlphaFormat: 0,
                                },
                            ) != 0
                        })
                    } else {
                        false
                    };
                    RestoreDC(dc, saved);
                    ok
                } else {
                    false
                };
                if !blended {
                    FillRgn(dc, region, brush);
                }
                SetDCBrushColor(dc, 0x64605b);
                FrameRgn(dc, region, brush, 1, 1);
                SetDCBrushColor(dc, 0x181818);
                DeleteObject(region);
            }
            if self.visible_bars[Bar::Bottom as usize] && self.frame.is_some() {
                let label = self
                    .reader
                    .zoom_readout((viewport.right, viewport.bottom), 1.0);
                let saved = SaveDC(dc);
                if saved != 0 {
                    SelectObject(dc, GetStockObject(DEFAULT_GUI_FONT));
                    SetBkMode(dc, TRANSPARENT as i32);
                    SetTextColor(dc, 0xfff3ed);
                    for (line, top) in label.lines().zip([readout.top + 6, readout.top + 24]) {
                        let text = wide(line);
                        let mut rect = RECT {
                            left: readout.left + 8,
                            top,
                            right: readout.right - 8,
                            bottom: top + 16,
                        };
                        DrawTextW(
                            dc,
                            text.as_ptr(),
                            text.len() as i32 - 1,
                            &mut rect,
                            DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_END_ELLIPSIS,
                        );
                    }
                    RestoreDC(dc, saved);
                }
            }
        }
    }

    // Down/up-sampling softens only the content behind a panel. One tiny DIB
    // is reused by all panels; native text and controls never enter the sample.
    unsafe fn draw_glass_sample(&mut self, dc: HDC, viewport: RECT, panel: RECT, dest: RECT) {
        const STEP: i32 = 10;
        if self.live_sizing || self.glass_pending || self.frame.is_none() {
            return;
        }
        let (w, h) = self.picture_dimensions(viewport);
        let (x, y) =
            self.reader
                .picture_origin((w, h), (viewport.right, viewport.bottom), self.pan);
        let sw = (panel.right - panel.left + STEP - 1) / STEP;
        let sh = (panel.bottom - panel.top + STEP - 1) / STEP;
        if !(1..=128).contains(&sw) || !(1..=128).contains(&sh) {
            return;
        }
        unsafe {
            PaintBuffer::ensure(&mut self.glass_sample, dc, sw, sh);
            let (Some(sample), Some(display)) = (&self.glass_sample, &self.frame) else {
                return;
            };
            let bounds = RECT {
                right: sw,
                bottom: sh,
                ..Default::default()
            };
            let saved = SaveDC(sample.dc);
            if saved == 0 {
                return;
            }
            IntersectClipRect(sample.dc, 0, 0, sw, sh);
            SetDCBrushColor(sample.dc, 0x202020);
            FillRect(sample.dc, &bounds, GetStockObject(DC_BRUSH));
            SetStretchBltMode(sample.dc, HALFTONE);
            SetBrushOrgEx(sample.dc, 0, 0, ptr::null_mut());
            let width = display.frame.info.width as i32;
            let height = display.frame.info.height as i32;
            StretchDIBits(
                sample.dc,
                (x - panel.left).div_euclid(STEP),
                (y - panel.top).div_euclid(STEP),
                (w / STEP).max(1),
                (h / STEP).max(1),
                0,
                0,
                width,
                height,
                display.bgra.as_ptr().cast(),
                &bitmap_info(width, height),
                DIB_RGB_COLORS,
                SRCCOPY,
            );
            RestoreDC(sample.dc, saved);
            SetStretchBltMode(dc, HALFTONE);
            SetBrushOrgEx(dc, 0, 0, ptr::null_mut());
            if StretchBlt(
                dc,
                dest.left,
                dest.top,
                dest.right - dest.left,
                dest.bottom - dest.top,
                sample.dc,
                0,
                0,
                sw,
                sh,
                SRCCOPY,
            ) != 0
            {
                self.glass_draw_count = self.glass_draw_count.saturating_add(1);
            }
        }
    }

    unsafe fn metadata_background(&mut self, dc: HDC) -> HBRUSH {
        unsafe {
            let Some((left, top, width, height)) = self.metadata_bounds.get() else {
                return GetStockObject(BLACK_BRUSH);
            };
            PaintBuffer::ensure(&mut self.metadata_glass, dc, width, height);
            PaintBuffer::ensure(&mut self.bar_tint, dc, 1, 1);
            let (Some(buffer), Some(tint)) = (&self.metadata_glass, &self.bar_tint) else {
                return GetStockObject(BLACK_BRUSH);
            };
            let (memory, bitmap, tint) = (buffer.dc, buffer.bitmap, tint.dc);
            let local = RECT {
                right: width,
                bottom: height,
                ..Default::default()
            };
            let panel = RECT {
                left,
                right: left + width,
                top,
                bottom: top + height,
            };
            SetDCBrushColor(memory, 0x23201a);
            let brush = GetStockObject(DC_BRUSH);
            FillRect(memory, &local, brush);
            self.draw_glass_sample(memory, self.viewport(), panel, local);
            SetDCBrushColor(tint, 0x23201a);
            FillRect(
                tint,
                &RECT {
                    right: 1,
                    bottom: 1,
                    ..Default::default()
                },
                brush,
            );
            GdiAlphaBlend(
                memory,
                0,
                0,
                width,
                height,
                tint,
                0,
                0,
                1,
                1,
                BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    BlendFlags: 0,
                    SourceConstantAlpha: 204,
                    AlphaFormat: 0,
                },
            );
            let pattern = CreatePatternBrush(bitmap);
            if pattern.is_null() {
                return GetStockObject(BLACK_BRUSH);
            }
            self.metadata_brush = Some(PatternBrush(pattern));
            SetBrushOrgEx(dc, 0, 0, ptr::null_mut());
            pattern
        }
    }
}

unsafe fn file_dialog(owner: HWND) -> Option<PathBuf> {
    unsafe {
        let mut buffer = vec![0u16; 32768];
        let filter:Vec<u16>="이미지 / ZIP / CBZ\0*.jpg;*.jpeg;*.png;*.apng;*.gif;*.agif;*.webp;*.bmp;*.zip;*.cbz\0모든 파일\0*.*\0\0".encode_utf16().collect();
        let mut dialog: OPENFILENAMEW = std::mem::zeroed();
        dialog.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
        dialog.hwndOwner = owner;
        dialog.lpstrFile = buffer.as_mut_ptr();
        dialog.nMaxFile = buffer.len() as u32;
        dialog.lpstrFilter = filter.as_ptr();
        dialog.Flags = OFN_FILEMUSTEXIST
            | OFN_PATHMUSTEXIST
            | OFN_EXPLORER
            | OFN_DONTADDTORECENT
            | OFN_NOCHANGEDIR;
        if GetOpenFileNameW(&mut dialog) == 0 {
            return None;
        }
        use std::os::windows::ffi::OsStringExt;
        let end = buffer.iter().position(|c| *c == 0)?;
        Some(std::ffi::OsString::from_wide(&buffer[..end]).into())
    }
}

unsafe fn open_menu(hwnd: HWND, location: LPARAM) -> Option<u16> {
    use windows_sys::core::w;
    // SAFETY: menu and owner stay on the current UI thread during the modal call.
    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return None;
        }
        for (id, caption) in [
            (OPEN, w!("파일 열기…")),
            (FOLDER, w!("폴더 열기…")),
            (CONNECT, w!("주소 열기…")),
            (VIEW_LOG, w!("로그 보기")),
        ] {
            AppendMenuW(menu, MF_STRING, usize::from(id), caption);
        }
        let mut point = POINT {
            x: location as i16 as i32,
            y: (location >> 16) as i16 as i32,
        };
        if location == -1 {
            let mut rect = RECT::default();
            GetClientRect(hwnd, &mut rect);
            point = POINT {
                x: rect.right / 2,
                y: rect.bottom / 2,
            };
            ClientToScreen(hwnd, &mut point);
        }
        let command = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
            point.x,
            point.y,
            0,
            hwnd,
            ptr::null(),
        );
        DestroyMenu(menu);
        (command != 0).then_some(command as u16)
    }
}

unsafe fn log_dialog(owner: HWND, log: &str) {
    use windows_sys::core::w;
    #[repr(C, align(4))]
    struct Template {
        dialog: DLGTEMPLATE,
        menu: u16,
        class: u16,
        title: [u16; 1],
    }
    let template = Template {
        dialog: DLGTEMPLATE {
            style: WS_POPUP | WS_CAPTION | WS_SYSMENU | DS_MODALFRAME as u32 | DS_CENTER as u32,
            dwExtendedStyle: 0,
            cdit: 0,
            x: 0,
            y: 0,
            cx: 370,
            cy: 210,
        },
        menu: 0,
        class: 0,
        title: [0],
    };
    unsafe extern "system" fn callback(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> isize {
        // SAFETY: controls are owned by this UI thread; the UTF-16 snapshot
        // lives throughout DialogBoxIndirectParamW and is copied by the edit.
        unsafe {
            match message {
                WM_INITDIALOG => {
                    SetWindowTextW(hwnd, w!("로그 보기"));
                    let instance = GetModuleHandleW(ptr::null());
                    let mut rect = RECT::default();
                    GetClientRect(hwnd, &mut rect);
                    let edit = CreateWindowExW(
                        WS_EX_CLIENTEDGE,
                        w!("EDIT"),
                        lparam as *const u16,
                        WS_CHILD
                            | WS_VISIBLE
                            | WS_TABSTOP
                            | WS_VSCROLL
                            | ES_MULTILINE as u32
                            | ES_READONLY as u32
                            | ES_AUTOVSCROLL as u32,
                        10,
                        10,
                        rect.right - 20,
                        rect.bottom - 52,
                        hwnd,
                        ptr::null_mut(),
                        instance,
                        ptr::null(),
                    );
                    let close = CreateWindowExW(
                        0,
                        w!("BUTTON"),
                        w!("닫기"),
                        WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_DEFPUSHBUTTON as u32,
                        rect.right - 82,
                        rect.bottom - 34,
                        72,
                        26,
                        hwnd,
                        IDOK as usize as _,
                        instance,
                        ptr::null(),
                    );
                    if edit.is_null() || close.is_null() {
                        EndDialog(hwnd, 0);
                        return 1;
                    }
                    let font = GetStockObject(DEFAULT_GUI_FONT) as usize;
                    SendMessageW(edit, WM_SETFONT, font, 1);
                    SendMessageW(close, WM_SETFONT, font, 1);
                    SendMessageW(hwnd, DM_SETDEFID, IDOK as usize, 0);
                    SetFocus(edit);
                    0
                }
                WM_CLOSE => {
                    EndDialog(hwnd, 0);
                    1
                }
                WM_COMMAND if matches!(wparam as u16 as i32, IDOK | IDCANCEL) => {
                    EndDialog(hwnd, 0);
                    1
                }
                _ => 0,
            }
        }
    }
    let text = wide(&log.replace('\n', "\r\n"));
    // SAFETY: the aligned template and NUL-terminated text live through the modal call.
    unsafe {
        DialogBoxIndirectParamW(
            GetModuleHandleW(ptr::null()),
            &raw const template.dialog,
            owner,
            Some(callback),
            text.as_ptr() as LPARAM,
        );
    }
}

struct AddressDialog {
    value: Option<String>,
    smoke: bool,
}

unsafe fn address_dialog(owner: HWND, smoke: bool) -> Option<String> {
    use windows_sys::core::w;
    #[repr(C, align(4))]
    struct Template {
        dialog: DLGTEMPLATE,
        menu: u16,
        class: u16,
        title: [u16; 1],
    }
    let mut state = AddressDialog { value: None, smoke };
    let template = Template {
        dialog: DLGTEMPLATE {
            style: WS_POPUP | WS_CAPTION | WS_SYSMENU | DS_MODALFRAME as u32,
            dwExtendedStyle: 0,
            cdit: 0,
            x: 0,
            y: 0,
            cx: 225,
            cy: 60,
        },
        menu: 0,
        class: 0,
        title: [0],
    };
    const EDIT_ADDRESS: i32 = 501;
    unsafe extern "system" fn callback(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> isize {
        // SAFETY: DialogBoxIndirectParamW owns the modal loop and retains the
        // stack state only until it returns. The edit only borrows local buffers.
        unsafe {
            if message == WM_INITDIALOG {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, lparam);
                SetWindowTextW(hwnd, w!("주소 열기"));
                let instance = GetModuleHandleW(ptr::null());
                let mut rect = RECT::default();
                GetClientRect(hwnd, &mut rect);
                let width = rect.right;
                let edit = CreateWindowExW(
                    WS_EX_CLIENTEDGE,
                    w!("EDIT"),
                    w!(""),
                    WS_CHILD | WS_VISIBLE | WS_TABSTOP | ES_AUTOHSCROLL as u32,
                    10,
                    12,
                    width - 20,
                    26,
                    hwnd,
                    EDIT_ADDRESS as usize as _,
                    instance,
                    ptr::null(),
                );
                if edit.is_null() {
                    EndDialog(hwnd, 0);
                    return 1;
                }
                SendMessageW(edit, EM_SETLIMITTEXT, 16384, 0);
                let font = GetStockObject(DEFAULT_GUI_FONT) as usize;
                SendMessageW(edit, WM_SETFONT, font, 1);
                for (id, caption, x, style) in [
                    (IDOK, w!("열기"), width - 158, BS_DEFPUSHBUTTON as u32),
                    (IDCANCEL, w!("취소"), width - 82, BS_PUSHBUTTON as u32),
                ] {
                    let button = CreateWindowExW(
                        0,
                        w!("BUTTON"),
                        caption,
                        WS_CHILD | WS_VISIBLE | WS_TABSTOP | style,
                        x,
                        47,
                        72,
                        26,
                        hwnd,
                        id as usize as _,
                        instance,
                        ptr::null(),
                    );
                    SendMessageW(button, WM_SETFONT, font, 1);
                }
                SendMessageW(hwnd, DM_SETDEFID, IDOK as usize, 0);
                SetFocus(edit);
                if (*(lparam as *const AddressDialog)).smoke {
                    SetWindowTextW(edit, w!("smb://server/share/book.cbz"));
                    PostMessageW(hwnd, WM_COMMAND, IDOK as usize, 0);
                }
                return 0;
            }
            if message == WM_CLOSE || (message == WM_COMMAND && wparam as u16 as i32 == IDCANCEL) {
                EndDialog(hwnd, 0);
                return 1;
            }
            if message == WM_COMMAND && wparam as u16 as i32 == IDOK {
                let edit = GetDlgItem(hwnd, EDIT_ADDRESS);
                let len = GetWindowTextLengthW(edit).clamp(0, 16384) as usize;
                let mut text = vec![0; len + 1];
                let read =
                    GetWindowTextW(edit, text.as_mut_ptr(), text.len() as i32).max(0) as usize;
                let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut AddressDialog;
                if !state.is_null() && read != 0 {
                    (*state).value = Some(String::from_utf16_lossy(&text[..read]));
                }
                EndDialog(hwnd, 1);
                return 1;
            }
            0
        }
    }
    // SAFETY: template is DWORD-aligned, contains no child templates, and
    // both it and the output stay alive during the modal Win32 call.
    unsafe {
        DialogBoxIndirectParamW(
            GetModuleHandleW(ptr::null()),
            &raw const template.dialog,
            owner,
            Some(callback),
            (&raw mut state) as LPARAM,
        );
    }
    state.value
}
unsafe fn folder_dialog(owner: HWND) -> Option<PathBuf> {
    unsafe {
        let title = wide("만화 폴더 열기");
        let info = BROWSEINFOW {
            hwndOwner: owner,
            lpszTitle: title.as_ptr(),
            ulFlags: BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE,
            ..std::mem::zeroed()
        };
        let item = SHBrowseForFolderW(&info);
        if item.is_null() {
            return None;
        }
        let mut buffer = vec![0u16; 32768];
        let valid = SHGetPathFromIDListEx(item, buffer.as_mut_ptr(), buffer.len() as u32, 0);
        CoTaskMemFree(item.cast());
        if valid == 0 {
            return None;
        }
        use std::os::windows::ffi::OsStringExt;
        let end = buffer.iter().position(|c| *c == 0)?;
        Some(std::ffi::OsString::from_wide(&buffer[..end]).into())
    }
}

fn copy_metadata_to_clipboard(owner: HWND, text: &str) -> bool {
    use windows_sys::Win32::{
        Foundation::GlobalFree,
        System::{DataExchange::*, Memory::*, Ole::CF_UNICODETEXT},
    };
    let text = wide(&text.replace('\n', "\r\n"));
    let bytes = text.len() * std::mem::size_of::<u16>();
    // SAFETY: this runs on the owning GUI thread; only a successful
    // SetClipboardData transfers ownership of the movable allocation to Windows.
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, bytes);
        if memory.is_null() {
            return false;
        }
        let buffer = GlobalLock(memory).cast::<u16>();
        let mut transferred = false;
        if !buffer.is_null() {
            ptr::copy_nonoverlapping(text.as_ptr(), buffer, text.len());
            GlobalUnlock(memory);
            if OpenClipboard(owner) != 0 {
                transferred = EmptyClipboard() != 0
                    && !SetClipboardData(u32::from(CF_UNICODETEXT), memory).is_null();
                CloseClipboard();
            }
        }
        if !transferred {
            GlobalFree(memory);
        }
        transferred
    }
}

unsafe fn draw_glass_button(item: &windows_sys::Win32::UI::Controls::DRAWITEMSTRUCT) {
    use windows_sys::Win32::UI::Controls::{ODS_DISABLED, ODS_FOCUS, ODS_SELECTED};
    // SAFETY: Win32 lends the drawing DC for this WM_DRAWITEM only.
    unsafe {
        let dc = item.hDC;
        let saved = SaveDC(dc);
        if saved == 0 {
            return;
        }
        let disabled = item.itemState & ODS_DISABLED != 0;
        let pressed = item.itemState & ODS_SELECTED != 0;
        let focused = item.itemState & ODS_FOCUS != 0;
        let mut cursor = POINT::default();
        let hover = GetCursorPos(&mut cursor) != 0
            && ScreenToClient(item.hwndItem, &mut cursor) != 0
            && PtInRect(&item.rcItem, cursor) != 0;
        SetDCBrushColor(dc, 0x28231c);
        let brush = GetStockObject(DC_BRUSH);
        FillRect(dc, &item.rcItem, brush);
        SelectObject(dc, brush);
        SelectObject(dc, GetStockObject(DC_PEN));
        SetDCPenColor(dc, if focused { 0xeee2c8 } else { 0x63574b });
        SetDCBrushColor(
            dc,
            if pressed {
                0x695b47
            } else if hover && !disabled {
                0x514332
            } else {
                0x3a3026
            },
        );
        let mut rect = item.rcItem;
        RoundRect(dc, rect.left, rect.top, rect.right, rect.bottom, 10, 10);
        if item.CtlID as u16 == COPY_METADATA {
            // Standard overlapping documents icon, drawn without a font dependency.
            let x = (rect.left + rect.right) / 2;
            let y = (rect.top + rect.bottom) / 2;
            SetDCPenColor(dc, 0xfff3ed);
            SelectObject(dc, GetStockObject(NULL_BRUSH));
            MoveToEx(dc, x - 1, y - 5, ptr::null_mut());
            LineTo(dc, x - 1, y - 8);
            LineTo(dc, x + 8, y - 8);
            LineTo(dc, x + 8, y + 3);
            LineTo(dc, x + 5, y + 3);
            Rectangle(dc, x - 6, y - 4, x + 5, y + 9);
            RestoreDC(dc, saved);
            return;
        }
        let mut label = [0u16; 40];
        let length =
            GetWindowTextW(item.hwndItem, label.as_mut_ptr(), label.len() as i32).max(0) as usize;
        let caption = String::from_utf16_lossy(&label[..length]);
        let glyph = match item.CtlID as u16 {
            PREVIOUS => "←",
            NEXT => "→",
            FIT => "⊡",
            ROTATE => "↻",
            FULLSCREEN => "⛶",
            FRAME_PREVIOUS => "‹",
            FRAME_NEXT => "›",
            PLAY if caption.starts_with("재생") => "▶",
            PLAY => "Ⅱ",
            TOP_PIN | BOTTOM_PIN if caption == "고정" => "◇",
            TOP_PIN | BOTTOM_PIN => "◆",
            METADATA => "ⓘ",
            _ => &caption,
        };
        SetBkMode(dc, TRANSPARENT as i32);
        SetTextColor(
            dc,
            if disabled {
                0xaaa098
            } else if item.CtlID as u16 == METADATA && caption == "메타데이터 닫기" {
                0xffd096
            } else {
                0xfff3ed
            },
        );
        let text = wide(glyph);
        DrawTextW(
            dc,
            text.as_ptr(),
            text.len() as i32 - 1,
            &mut rect,
            DT_SINGLELINE | DT_VCENTER | DT_CENTER | DT_NOPREFIX,
        );
        RestoreDC(dc, saved);
    }
}

unsafe fn attach_tooltips(
    owner: HWND,
    instance: windows_sys::Win32::Foundation::HINSTANCE,
    controls: &[(u16, HWND, i32, i32)],
    metadata_copy: HWND,
) {
    use windows_sys::{Win32::UI::Controls::*, core::w};
    // SAFETY: the owned tooltip window and its tools live on the UI thread.
    // All tool strings below have static storage.
    unsafe {
        if InitCommonControlsEx(&INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_WIN95_CLASSES,
        }) == 0
        {
            return;
        }
        let tooltip = CreateWindowExW(
            WS_EX_TOPMOST,
            TOOLTIPS_CLASSW,
            ptr::null(),
            WS_POPUP | TTS_NOPREFIX,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            owner,
            ptr::null_mut(),
            instance,
            ptr::null(),
        );
        if tooltip.is_null() {
            return;
        }
        for (id, control, _, _) in
            controls
                .iter()
                .copied()
                .chain([(COPY_METADATA, metadata_copy, 0, 0)])
        {
            let label = match id {
                PREVIOUS => w!("이전 페이지"),
                NEXT => w!("다음 페이지"),
                SMALLER => w!("축소"),
                LARGER => w!("확대"),
                FIT => w!("맞춤"),
                ROTATE => w!("회전"),
                FULLSCREEN => w!("전체 화면"),
                TOP_PIN | BOTTOM_PIN => w!("고정 전환"),
                METADATA => w!("메타데이터 · Tab"),
                COPY_METADATA => w!("메타데이터 복사"),
                FRAME_PREVIOUS => w!("이전 프레임"),
                FRAME_NEXT => w!("다음 프레임"),
                PLAY => w!("재생 / 일시정지"),
                SPEED => w!("배속"),
                _ => continue,
            };
            let tool = TTTOOLINFOW {
                cbSize: std::mem::size_of::<TTTOOLINFOW>() as u32,
                uFlags: TTF_IDISHWND | TTF_SUBCLASS,
                hwnd: owner,
                uId: control as usize,
                lpszText: label.cast_mut(),
                ..Default::default()
            };
            SendMessageW(tooltip, TTM_ADDTOOLW, 0, (&raw const tool) as LPARAM);
        }
    }
}

unsafe fn live_resize_smoke(hwnd: HWND, state: &RefCell<App>, check_pixels: bool) {
    // SAFETY: this is driven only by the smoke timer on the owning UI thread.
    // Release the App borrow before moving the window so WM_SIZE can paint.
    unsafe {
        let control;
        {
            let mut app = state.borrow_mut();
            if !app.bars.pinned(Bar::Bottom) {
                app.bars.toggle_pin(Bar::Bottom);
                app.sync_pins();
            }
            control = app.control(SMALLER);
        }
        let button_probe = || {
            let mut rect = RECT::default();
            GetWindowRect(control, &mut rect);
            let mut probe = POINT {
                x: rect.left + 6,
                y: rect.top + 14,
            };
            ScreenToClient(hwnd, &mut probe);
            probe
        };
        let probe = button_probe();
        RedrawWindow(
            hwnd,
            ptr::null(),
            ptr::null_mut(),
            RDW_INVALIDATE | RDW_ALLCHILDREN | RDW_NOERASE | RDW_UPDATENOW,
        );
        let pixel = |probe: POINT| {
            GdiFlush();
            // GetPixel on the screen reads the presented desktop, not GDI's
            // queued writes. Wait for the compositor only in this smoke test.
            if windows_sys::Win32::Graphics::Dwm::DwmFlush() < 0 {
                return None;
            }
            let mut point = probe;
            ClientToScreen(hwnd, &mut point);
            let hit = WindowFromPoint(point);
            if hit != hwnd && IsChild(hwnd, hit) == 0 {
                return None;
            }
            let dc = GetDC(ptr::null_mut());
            let color = GetPixel(dc, point.x, point.y);
            ReleaseDC(ptr::null_mut(), dc);
            (color != CLR_INVALID).then_some(color)
        };
        let previous = check_pixels.then(|| pixel(probe)).flatten();
        SendMessageW(hwnd, WM_ENTERSIZEMOVE, 0, 0);
        let mut sizing = RECT::default();
        GetWindowRect(hwnd, &mut sizing);
        sizing.right += 100;
        sizing.bottom += 100;
        SendMessageW(
            hwnd,
            WM_SIZING,
            WMSZ_BOTTOMRIGHT as usize,
            (&raw mut sizing) as LPARAM,
        );
        let mut valid = true;
        for (index, (width, height)) in [(940, 700), (780, 470), (720, 300), (860, 570)]
            .into_iter()
            .enumerate()
        {
            SetWindowPos(
                hwnd,
                ptr::null_mut(),
                0,
                0,
                width,
                height,
                SWP_NOMOVE | SWP_NOZORDER,
            );
            let app = state.borrow();
            let step_valid = app.visible_bars == [false, false]
                && !app.visible_animation
                && app
                    .controls
                    .iter()
                    .all(|&(_, control, _, _)| IsWindowVisible(control) == 0);
            if !step_valid {
                eprintln!(
                    "nCatImage: live resize at {width}×{height}: controls {:?}, animated {}",
                    app.visible_bars, app.visible_animation
                );
            }
            valid &= step_valid;
            if index == 0
                && let Some(previous) = previous
                && let Some(next) = pixel(probe)
            {
                if previous == 0x202020 || next != 0x202020 {
                    eprintln!(
                        "nCatImage: former button pixel during resize: {previous:#x} -> {next:#x}"
                    );
                    valid = false;
                } else {
                    println!("live_resize_pixels=ok old_button_cleared=true");
                }
            } else if index == 0 && check_pixels {
                println!("live_resize_pixels=skipped desktop probe not visible");
            }
        }
        SendMessageW(hwnd, WM_EXITSIZEMOVE, 0, 0);
        if !state.borrow().visible_bars[Bar::Bottom as usize] {
            eprintln!("nCatImage: resize did not restore the pinned bottom panel");
            valid = false;
        }
        // Also grow the window without entering the modal sizing loop. Keep
        // the controls visible and check each vacated location after the
        // following resize, as well as all of them after the final resize.
        if check_pixels {
            let mut former = Vec::new();
            let mut count = 0;
            for (width, height) in [(892, 602), (924, 634), (956, 666)] {
                let point = button_probe();
                if let Some(color) = pixel(point)
                    && color != 0x202020
                {
                    former.push(point);
                }
                SetWindowPos(
                    hwnd,
                    ptr::null_mut(),
                    0,
                    0,
                    width,
                    height,
                    SWP_NOMOVE | SWP_NOZORDER,
                );
                let mut msg = MSG::default();
                while PeekMessageW(
                    &mut msg,
                    hwnd,
                    VIEWPORT_CHANGED,
                    VIEWPORT_CHANGED,
                    PM_REMOVE,
                ) != 0
                {
                    DispatchMessageW(&msg);
                }
                while PeekMessageW(&mut msg, hwnd, WM_PAINT, WM_PAINT, PM_REMOVE) != 0 {
                    DispatchMessageW(&msg);
                }
                {
                    let app = state.borrow();
                    valid &= app.bar_geometry_matches() && app.visible_bars[Bar::Bottom as usize];
                }
                for &point in &former {
                    if let Some(color) = pixel(point) {
                        count += 1;
                        if color != 0x202020 {
                            eprintln!(
                                "nCatImage: stacked button trail at ({}, {}): {color:#x}",
                                point.x, point.y
                            );
                            valid = false;
                        }
                    }
                }
            }
            println!(
                "visible_resize_pixels={} probes={count}",
                if count > 0 && valid {
                    "ok"
                } else if count == 0 {
                    "skipped"
                } else {
                    "failed"
                }
            );
        }
        if !valid {
            eprintln!("nCatImage: live resize left a control visible, stale, or unrestored");
            state.borrow_mut().close();
        }
    }
}

unsafe extern "system" fn procedure(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: pointer targets the run-owned RefCell, which outlives every HWND.
    // Reentrant child notifications never create a second mutable App reference.
    unsafe {
        if message == WM_NCCREATE {
            let create = &*(lparam as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        }
        if message == WM_DESTROY {
            PostQuitMessage(0);
            return 0;
        }
        if message == WM_ERASEBKGND {
            return 1;
        }
        if message == WM_WINDOWPOSCHANGING {
            let pos = &mut *(lparam as *mut WINDOWPOS);
            if pos.flags & SWP_NOSIZE == 0 {
                pos.flags |= SWP_NOCOPYBITS;
            }
        }
        if message == WM_SIZING {
            let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<App>;
            let mut hidden = false;
            if !state.is_null()
                && let Ok(mut app) = (*state).try_borrow_mut()
                && !app.live_sizing
            {
                // Hide native children at their old locations, before USER32
                // changes the client surface for the first live resize step.
                app.live_sizing = true;
                app.glass_pending = true;
                app.layout();
                hidden = true;
            }
            if hidden {
                RedrawWindow(
                    hwnd,
                    ptr::null(),
                    ptr::null_mut(),
                    RDW_INVALIDATE | RDW_ALLCHILDREN | RDW_NOERASE | RDW_UPDATENOW,
                );
            }
            return 1;
        }
        if message == WM_NCCALCSIZE && wparam != 0 {
            // The image and floating bars are anchored differently. No region
            // from the old client surface is valid to copy when its size changes.
            DefWindowProcW(hwnd, message, wparam, lparam);
            return WVR_REDRAW as LRESULT;
        }
        if message == WM_GETMINMAXINFO {
            let info = &mut *(lparam as *mut MINMAXINFO);
            info.ptMinTrackSize.x = info
                .ptMinTrackSize
                .x
                .max(ANIMATION_SIZE.0 + BOTTOM_SIZE.0 + ZOOM_READOUT_WIDTH + 40);
            info.ptMinTrackSize.y = info.ptMinTrackSize.y.max(260);
            return 0;
        }
        if message == WM_DPICHANGED && lparam != 0 {
            // USER32 supplies device-pixel window bounds. Do not hold an App
            // borrow across SetWindowPos: it can synchronously send WM_SIZE.
            let mut bounds = *(lparam as *const RECT);
            let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<App>;
            let fullscreen = !state.is_null()
                && (*state)
                    .try_borrow()
                    .is_ok_and(|app| app.fullscreen.is_some());
            if fullscreen
                && let Some(monitor) =
                    monitor_info(MonitorFromRect(&bounds, MONITOR_DEFAULTTONEAREST))
            {
                bounds = monitor.rcMonitor;
            }
            SetWindowPos(
                hwnd,
                ptr::null_mut(),
                bounds.left,
                bounds.top,
                bounds.right - bounds.left,
                bounds.bottom - bounds.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            PostMessageW(hwnd, VIEWPORT_CHANGED, 0, 0);
            return 0;
        }
        if message == WM_DRAWITEM {
            let item = &*(lparam as *const windows_sys::Win32::UI::Controls::DRAWITEMSTRUCT);
            if item.CtlType == windows_sys::Win32::UI::Controls::ODT_BUTTON {
                draw_glass_button(item);
                return 1;
            }
        }
        if matches!(message, WM_SIZE | WM_ENTERSIZEMOVE | WM_EXITSIZEMOVE) {
            // Interactive sizing can repaint before the posted viewport update.
            // Align controls now when possible, but keep decoding deferred.
            let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<App>;
            let mut repaint_now = false;
            if !state.is_null()
                && let Ok(mut app) = (*state).try_borrow_mut()
            {
                if message == WM_ENTERSIZEMOVE {
                    app.in_size_move = true;
                } else if message == WM_EXITSIZEMOVE {
                    app.in_size_move = false;
                    repaint_now = app.live_sizing;
                    app.live_sizing = false;
                    if repaint_now {
                        app.glass_pending = true;
                        app.layout();
                    }
                } else if app.in_size_move
                    && !app.controls.is_empty()
                    && wparam != SIZE_MINIMIZED as usize
                {
                    app.live_sizing = true;
                    app.glass_pending = true;
                    app.layout();
                    repaint_now = true;
                }
            }
            if message == WM_ENTERSIZEMOVE {
                return 0;
            }
            InvalidateRect(hwnd, ptr::null(), 0);
            PostMessageW(
                hwnd,
                VIEWPORT_CHANGED,
                if message == WM_EXITSIZEMOVE && repaint_now {
                    VIEWPORT_RESIZE_FINISHED
                } else if message == WM_SIZE && wparam == SIZE_MINIMIZED as usize {
                    VIEWPORT_MINIMIZED
                } else {
                    0
                },
                0,
            );
            // After dropping the App borrow, present the new viewport and
            // native controls before USER32 can show an intermediate resize.
            if repaint_now {
                RedrawWindow(
                    hwnd,
                    ptr::null(),
                    ptr::null_mut(),
                    RDW_INVALIDATE | RDW_ALLCHILDREN | RDW_NOERASE | RDW_UPDATENOW,
                );
            }
            return 0;
        }
        let data = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<App>;
        if data.is_null() {
            return DefWindowProcW(hwnd, message, wparam, lparam);
        }
        if message == LIVE_RESIZE_TEST {
            let smoke = (*data).borrow().smoke;
            if matches!(smoke, Smoke::Resize | Smoke::ResizeFrame) {
                live_resize_smoke(hwnd, &*data, smoke == Smoke::Resize);
            }
            return 0;
        }
        let Ok(mut app) = (*data).try_borrow_mut() else {
            return DefWindowProcW(hwnd, message, wparam, lparam);
        };
        // Creation messages precede CreateWindowExW returning to run.
        app.hwnd = hwnd;
        match message {
            WM_PAINT => {
                app.paint();
                0
            }
            WM_CTLCOLORSTATIC if lparam as HWND == app.metadata_hwnd => {
                let dc = wparam as windows_sys::Win32::Graphics::Gdi::HDC;
                SetTextColor(dc, 0x00ffffff);
                SetBkMode(dc, TRANSPARENT as i32);
                app.metadata_background(dc) as isize
            }
            VIEWPORT_CHANGED => {
                // A synchronous WM_PAINT during a window transition can consume
                // the initial invalidation while App is still borrowed.
                app.invalidate_picture();
                app.glass_pending = true;
                SetTimer(hwnd, GLASS_SETTLE, 120, None);
                if wparam & VIEWPORT_MINIMIZED != 0 {
                    app.bar_layout_size = None;
                }
                app.viewport_changed();
                if wparam & VIEWPORT_RESIZE_FINISHED != 0 {
                    // The final native resize can invalidate a different set
                    // of client rectangles than the last WM_SIZING step.
                    drop(app);
                    RedrawWindow(
                        hwnd,
                        ptr::null(),
                        ptr::null_mut(),
                        RDW_INVALIDATE | RDW_ALLCHILDREN | RDW_ERASE | RDW_UPDATENOW,
                    );
                }
                0
            }
            WM_COMMAND => {
                app.command((wparam & 0xffff) as u16, ((wparam >> 16) & 0xffff) as u16);
                0
            }
            WM_CONTEXTMENU => {
                if let Some(id) = open_menu(hwnd, lparam) {
                    app.command(id, 0);
                }
                0
            }
            // Wake only. A modal loop can dispatch this while App is borrowed;
            // the outer message loop drains the data after the modal call returns.
            RESULT => 0,
            WM_TIMER => {
                if wparam == GLASS_SETTLE {
                    KillTimer(hwnd, GLASS_SETTLE);
                    app.glass_pending = false;
                    app.refresh_glass();
                } else if wparam == METADATA_TEST {
                    app.metadata_test();
                } else if wparam == RESIZE_TEST {
                    app.resize_test();
                } else if wparam == ANIMATION {
                    KillTimer(hwnd, ANIMATION);
                    app.animation(None);
                } else if wparam == DEADLINE {
                    if app.smoke == Smoke::Window {
                        let context = GetWindowDpiAwarenessContext(hwnd);
                        app.success = GetAwarenessFromDpiAwarenessContext(context)
                            == DPI_AWARENESS_PER_MONITOR_AWARE;
                        if app.success {
                            // Wine can report v1 for windows created in a v2
                            // process. The native smoke separately requires v2.
                            let v2 = AreDpiAwarenessContextsEqual(
                                context,
                                DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
                            ) != 0;
                            println!(
                                "window_dpi_smoke=ok per_monitor_v2={v2} dpi={}",
                                window_dpi(hwnd)
                            );
                        } else {
                            eprintln!("nCatImage: viewer is not per-monitor DPI aware");
                        }
                    }
                    app.close();
                } else if wparam == SHUTDOWN
                    && (app.backend.is_closed()
                        || app
                            .closing
                            .is_some_and(|at| at.elapsed() > Duration::from_millis(1500)))
                {
                    KillTimer(hwnd, SHUTDOWN);
                    DestroyWindow(hwnd);
                }
                0
            }
            WM_CLOSE => {
                app.close();
                0
            }
            WM_LBUTTONDOWN => {
                app.controls_input = false;
                SetFocus(hwnd);
                app.layout();
                let viewport = app.viewport();
                let picture = app.picture_dimensions(viewport);
                if app.frame.is_some()
                    && !app.reader.fit
                    && (picture.0 > viewport.right || picture.1 > viewport.bottom)
                {
                    app.drag = Some((
                        (lparam as u16) as i16 as i32,
                        ((lparam >> 16) as u16) as i16 as i32,
                    ));
                    SetCapture(hwnd);
                }
                0
            }
            WM_LBUTTONUP => {
                app.drag = None;
                ReleaseCapture();
                0
            }
            WM_MOUSEMOVE => {
                if let Some((x, y)) = app.drag {
                    let point = (
                        (lparam as u16) as i16 as i32,
                        ((lparam >> 16) as u16) as i16 as i32,
                    );
                    app.pan.0 = app.pan.0.saturating_add(point.0 - x);
                    app.pan.1 = app.pan.1.saturating_add(point.1 - y);
                    app.drag = Some(point);
                    app.refresh_glass();
                }
                0
            }
            WM_MOUSEWHEEL => {
                let delta = ((wparam >> 16) as u16) as i16 as f64;
                if GetKeyState(VK_CONTROL as i32) < 0 {
                    app.zoom(if delta > 0.0 { 1.1 } else { 1.0 / 1.1 });
                } else if !app.reader.fit && app.reader.zoom > 1.0 {
                    app.pan.1 = app.pan.1.saturating_add((delta / 2.0) as i32);
                    app.refresh_glass();
                } else {
                    if app.wheel.signum() != delta.signum() {
                        app.wheel = delta;
                    } else {
                        app.wheel += delta;
                    }
                    if app.wheel.abs() >= 120.0
                        && app
                            .last_wheel
                            .is_none_or(|at| at.elapsed() >= Duration::from_millis(100))
                    {
                        let forward = app.wheel < 0.0;
                        app.wheel = 0.0;
                        app.last_wheel = Some(Instant::now());
                        app.navigate(forward);
                    }
                }
                0
            }
            WM_DROPFILES => {
                let drop = wparam as windows_sys::Win32::UI::Shell::HDROP;
                let count = DragQueryFileW(drop, u32::MAX, ptr::null_mut(), 0);
                let path = if count == 1 {
                    let len = DragQueryFileW(drop, 0, ptr::null_mut(), 0) as usize;
                    if len < ncat_protocol::MAX_PATH_BYTES / 2 {
                        let mut buffer = vec![0u16; len + 1];
                        DragQueryFileW(drop, 0, buffer.as_mut_ptr(), buffer.len() as u32);
                        use std::os::windows::ffi::OsStringExt;
                        Some(PathBuf::from(std::ffi::OsString::from_wide(&buffer[..len])))
                    } else {
                        None
                    }
                } else {
                    None
                };
                DragFinish(drop);
                if let Some(path) = path {
                    app.open(path);
                } else {
                    app.set_status("파일 또는 폴더를 한 개씩 드롭하세요");
                }
                0
            }
            _ => {
                // WM_NCLBUTTONDOWN / WM_SYSCOMMAND may run a nested USER32
                // move/resize loop until the mouse is released. Do not hold
                // App across it: WM_SIZING and WM_PAINT must be able to repaint.
                drop(app);
                DefWindowProcW(hwnd, message, wparam, lparam)
            }
        }
    }
}

pub fn run(initial: Option<PathBuf>, smoke: Smoke) -> io::Result<()> {
    let state_path = (smoke == Smoke::None)
        .then(window_state::state_path)
        .flatten();
    let restored = state_path.as_deref().and_then(window_state::load);
    let (app, sender) = App::new(smoke)?;
    let state = Box::new(RefCell::new(app));
    let class_name = wide("nCatImage.NativeViewer");
    let title = wide(concat!(
        "nCatImage v",
        env!("CARGO_PKG_VERSION"),
        " [UNTESTED Windows]"
    ));
    // SAFETY: static callback and run-owned state; synchronous calls use live,
    // NUL-terminated buffers. Shared bridges pass HWND integers, never App pointers.
    unsafe {
        // Opt out of OS bitmap stretching before creating any GUI windows.
        // The decoder worker never enters this path.
        let dpi_context = DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2;
        if SetProcessDpiAwarenessContext(dpi_context) == 0
            && AreDpiAwarenessContextsEqual(GetThreadDpiAwarenessContext(), dpi_context) == 0
        {
            eprintln!("nCatImage: per-monitor DPI awareness unavailable");
        }
        let com = CoInitializeEx(ptr::null(), COINIT_APARTMENTTHREADED as u32);
        let instance = GetModuleHandleW(ptr::null());
        let class = WNDCLASSW {
            lpfnWndProc: Some(procedure),
            hInstance: instance,
            lpszClassName: class_name.as_ptr(),
            hCursor: LoadCursorW(ptr::null_mut(), IDC_ARROW),
            ..std::mem::zeroed()
        };
        if RegisterClassW(&class) == 0 {
            if com >= 0 {
                CoUninitialize();
            }
            return Err(io::Error::last_os_error());
        }
        let result = (|| {
            let hwnd = CreateWindowExW(
                WS_EX_COMPOSITED,
                class_name.as_ptr(),
                title.as_ptr(),
                WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                1040,
                820,
                ptr::null_mut(),
                ptr::null_mut(),
                instance,
                (&*state as *const RefCell<App>).cast(),
            );
            if hwnd.is_null() {
                return Err(io::Error::last_os_error());
            }
            let _window = Window(hwnd);
            {
                let size = restored.unwrap_or(WindowSize {
                    width: 1040,
                    height: 820,
                });
                let mut rect = RECT::default();
                if let Some(work) = work_area(hwnd).filter(|_| GetWindowRect(hwnd, &mut rect) != 0)
                {
                    let dpi = window_dpi(hwnd);
                    position_in_work_area(
                        hwnd,
                        rect,
                        work,
                        dpi_units(size.width, BASE_DPI, dpi).min(work.right - work.left),
                        dpi_units(size.height, BASE_DPI, dpi).min(work.bottom - work.top),
                    );
                }
            }
            {
                let mut app = state.borrow_mut();
                app.hwnd = hwnd;
                app.remember_normal_size();
                let specs = [
                    (PREVIOUS, "BUTTON", "이전 페이지", 8, 8, 36),
                    (PAGE, "EDIT", "1", 48, 8, 52),
                    (PAGE_GO, "BUTTON", "이동", 104, 8, 46),
                    (NEXT, "BUTTON", "다음 페이지", 154, 8, 36),
                    (DIRECTION, "BUTTON", "좌→우", 194, 8, 60),
                    (TOP_PIN, "BUTTON", "고정 해제", 258, 8, 40),
                    (SMALLER, "BUTTON", "−", 8, 8, 36),
                    (FIT, "BUTTON", "맞춤", 48, 8, 36),
                    (LARGER, "BUTTON", "+", 88, 8, 36),
                    (ROTATE, "BUTTON", "회전", 128, 8, 36),
                    (FULLSCREEN, "BUTTON", "전체 화면", 168, 8, 36),
                    (BOTTOM_PIN, "BUTTON", "고정 해제", 208, 8, 36),
                    (FRAME_PREVIOUS, "BUTTON", "이전 프레임", 8, 8, 36),
                    (PLAY, "BUTTON", "일시정지", 48, 8, 36),
                    (FRAME_NEXT, "BUTTON", "다음 프레임", 88, 8, 36),
                    (SPEED, "COMBOBOX", "", 128, 8, 62),
                    (FRAME_NUMBER, "EDIT", "1", 194, 8, 58),
                    (FRAME_GO, "BUTTON", "이동", 256, 8, 54),
                    (METADATA, "BUTTON", "메타데이터 열기", 8, 8, 36),
                ];
                for (id, kind, text, x, y, width) in specs {
                    let style = WS_CHILD
                        | WS_TABSTOP
                        | if kind == "EDIT" {
                            WS_BORDER | ES_AUTOHSCROLL as u32
                        } else if kind == "COMBOBOX" {
                            CBS_DROPDOWNLIST as u32 | WS_VSCROLL
                        } else if kind == "BUTTON" {
                            BS_OWNERDRAW as u32
                        } else {
                            0
                        };
                    let control = CreateWindowExW(
                        0,
                        wide(kind).as_ptr(),
                        wide(text).as_ptr(),
                        style,
                        x,
                        y,
                        width,
                        if kind == "COMBOBOX" { 280 } else { 28 },
                        hwnd,
                        id as usize as _,
                        instance,
                        ptr::null(),
                    );
                    if control.is_null() {
                        DestroyWindow(hwnd);
                        return Err(io::Error::last_os_error());
                    }
                    SendMessageW(
                        control,
                        WM_SETFONT,
                        GetStockObject(DEFAULT_GUI_FONT) as usize,
                        1,
                    );
                    app.controls.push((id, control, x, y));
                }
                for speed in SPEEDS {
                    SendMessageW(
                        app.control(SPEED),
                        CB_ADDSTRING,
                        0,
                        wide(&format!("{speed}×")).as_ptr() as isize,
                    );
                }
                app.metadata_hwnd = CreateWindowExW(
                    0,
                    wide("EDIT").as_ptr(),
                    wide("").as_ptr(),
                    WS_CHILD
                        | WS_VSCROLL
                        | WS_BORDER
                        | ES_MULTILINE as u32
                        | ES_READONLY as u32
                        | ES_AUTOVSCROLL as u32,
                    12,
                    116,
                    480,
                    300,
                    hwnd,
                    ptr::null_mut(),
                    instance,
                    ptr::null(),
                );
                app.metadata_copy = CreateWindowExW(
                    0,
                    windows_sys::core::w!("BUTTON"),
                    windows_sys::core::w!("메타데이터 복사"),
                    WS_CHILD | WS_TABSTOP | BS_OWNERDRAW as u32,
                    0,
                    0,
                    32,
                    28,
                    hwnd,
                    COPY_METADATA as usize as _,
                    instance,
                    ptr::null(),
                );
                if app.metadata_hwnd.is_null() || app.metadata_copy.is_null() {
                    return Err(io::Error::last_os_error());
                }
                attach_tooltips(hwnd, instance, &app.controls, app.metadata_copy);
                SendMessageW(
                    app.metadata_hwnd,
                    WM_SETFONT,
                    GetStockObject(DEFAULT_GUI_FONT) as usize,
                    1,
                );
                SendMessageW(app.control(SPEED), CB_SETCURSEL, 3, 0);
                app.playback_ui();
                app.navigation_ui();
                DragAcceptFiles(hwnd, 1);
                app.layout();
            }
            let backend = state.borrow().backend.clone();
            let receiver = backend.events();
            let stop = state.borrow().stop.clone();
            let window = hwnd as usize;
            std::thread::Builder::new()
                .name("ncat-native-events".into())
                .spawn(move || {
                    while let Ok(event) = receiver.recv_blocking() {
                        if stop.load(Ordering::Acquire) {
                            break;
                        }
                        if !backend.accepts(&event) {
                            continue;
                        }
                        let mut event = stage(event);
                        loop {
                            if stop.load(Ordering::Acquire) {
                                return;
                            }
                            match sender.try_send(event) {
                                Ok(()) => break,
                                Err(mpsc::TrySendError::Full(back)) => {
                                    event = back;
                                    std::thread::sleep(Duration::from_millis(2));
                                }
                                Err(mpsc::TrySendError::Disconnected(_)) => return,
                            }
                        }
                        while PostMessageW(window as HWND, RESULT, 0, 0) == 0 {
                            if GetLastError() != ERROR_NOT_ENOUGH_QUOTA
                                || stop.load(Ordering::Acquire)
                            {
                                return;
                            }
                            std::thread::sleep(Duration::from_millis(2));
                        }
                    }
                })?;
            ShowWindow(hwnd, SW_SHOW);
            UpdateWindow(hwnd);
            if smoke == Smoke::Resize {
                SetTimer(hwnd, RESIZE_TEST, 250, None);
            }
            if smoke == Smoke::MetadataWindow {
                SetTimer(hwnd, METADATA_TEST, 300, None);
            }
            if smoke != Smoke::None {
                SetTimer(
                    hwnd,
                    DEADLINE,
                    if smoke == Smoke::Window {
                        350
                    } else if smoke == Smoke::Navigation {
                        60000
                    } else {
                        20000
                    },
                    None,
                );
            }
            if let Some(path) = initial {
                let mut app = state.borrow_mut();
                app.startup_image = true;
                app.open_impl(path);
            }
            let mut message: MSG = std::mem::zeroed();
            loop {
                {
                    let mut app = state.borrow_mut();
                    // Check the queue itself before blocking, even if a nested
                    // message loop already removed the only RESULT wake.
                    while let Ok(event) = app.events.try_recv() {
                        app.consume(event);
                    }
                }
                let status = GetMessageW(&mut message, ptr::null_mut(), 0, 0);
                if status < 0 {
                    return Err(io::Error::last_os_error());
                }
                if status == 0 {
                    break;
                }
                if message.message == WM_LBUTTONDOWN && message.hwnd != hwnd {
                    state.borrow_mut().controls_input = true;
                }
                if matches!(message.message, WM_MOUSEMOVE | WM_MOUSELEAVE)
                    && (message.hwnd == hwnd || IsChild(hwnd, message.hwnd) != 0)
                {
                    state
                        .borrow_mut()
                        .track_bars(message.hwnd, message.message == WM_MOUSELEAVE);
                }
                if message.message == WM_KEYDOWN && state.borrow_mut().key(message.wParam as u32) {
                    continue;
                }
                let controls_input = state.borrow().controls_input;
                if controls_input && IsDialogMessageW(hwnd, &message) != 0 {
                    continue;
                }
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            state.borrow().backend.shutdown();
            if smoke != Smoke::None && !state.borrow().success {
                return Err(io::Error::other("native GUI smoke failed"));
            }
            if smoke != Smoke::None {
                println!("native_gui_smoke=ok frames={}", state.borrow().smoke_frames);
            }
            Ok(())
        })();
        state.borrow().stop.store(true, Ordering::Release);
        state.borrow().backend.shutdown();
        UnregisterClassW(class_name.as_ptr(), instance);
        if com >= 0 {
            CoUninitialize();
        }
        if result.is_ok()
            && let (Some(path), Some(size)) = (state_path, state.borrow().normal_size)
            && window_state::save(&path, size).is_err()
        {
            eprintln!("nCatImage: could not save window size");
        }
        result
    }
}
