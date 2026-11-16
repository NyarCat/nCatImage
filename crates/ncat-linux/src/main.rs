#![forbid(unsafe_code)]
// Release builds are GUI-only on Windows (no console); debug builds keep stderr visible.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod network;

use gtk::{gdk, gio, glib, prelude::*};
use ncat_backend::{
    Backend, Book, DropEvent, DropRejection, Event, FrameBytes, NetworkFailure, View, worker_path,
};
use ncat_core::{Direction, fit_size, physical_next};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

fn interface_shown(enabled: bool, fullscreen: bool) -> bool {
    enabled && !fullscreen
}

/// Largest size with the image's aspect ratio that fits the area, enlarging small images.
fn contain_size(width: u32, height: u32, area_width: u32, area_height: u32) -> (u32, u32) {
    let (width, height) = (width.max(1), height.max(1));
    let scale = (f64::from(area_width.max(1)) / f64::from(width))
        .min(f64::from(area_height.max(1)) / f64::from(height));
    (
        (f64::from(width) * scale).floor().max(1.0) as u32,
        (f64::from(height) * scale).floor().max(1.0) as u32,
    )
}

fn page_step(key: gdk::Key) -> Option<bool> {
    match key {
        gdk::Key::space | gdk::Key::Page_Down | gdk::Key::Down | gdk::Key::KP_Down => Some(true),
        gdk::Key::BackSpace | gdk::Key::Page_Up | gdk::Key::Up | gdk::Key::KP_Up => Some(false),
        _ => None,
    }
}

fn single_drop_file(files: &[gio::File]) -> Result<&gio::File, DropRejection> {
    match files {
        [] => Err(DropRejection::Empty),
        [file] => Ok(file),
        _ => Err(DropRejection::MultipleItems),
    }
}

struct Ui {
    window: gtk::ApplicationWindow,
    header: gtk::HeaderBar,
    toolbar: gtk::Box,
    interface_visible: Cell<bool>,
    interface_input: Cell<bool>,
    drop_generation: Cell<Option<u64>>,
    network_request: Cell<u64>,
    network_task: RefCell<Option<glib::JoinHandle<()>>>,
    network_timer: RefCell<Option<glib::SourceId>>,
    picture: gtk::Picture,
    scroll: gtk::ScrolledWindow,
    status: gtk::Label,
    position: gtk::SpinButton,
    previous: gtk::Button,
    next: gtk::Button,
    direction_button: gtk::ToggleButton,
    /// 채우기 mode: enlarge to the viewer size keeping the aspect ratio. Persists across pages.
    fill_button: gtk::ToggleButton,
    fill: Cell<bool>,
    backend: Backend,
    book: RefCell<Option<Arc<Book>>>,
    selected: Cell<usize>,
    original: Cell<(u32, u32)>,
    zoom: Cell<f64>,
    quarter_turns: Cell<u8>,
    syncing: Cell<bool>,
    closing: Cell<bool>,
    reload_timer: RefCell<Option<glib::SourceId>>,
    loading_timer: RefCell<Option<glib::SourceId>>,
    smoke: bool,
    smoke_steps: Cell<Option<u32>>,
    smoke_started: Cell<bool>,
    smoke_ok: Rc<Cell<bool>>,
}

impl Ui {
    fn focus_viewer(&self) {
        self.interface_input.set(false);
        self.picture.grab_focus();
    }

    fn update_interface(&self) {
        let visible = interface_shown(self.interface_visible.get(), self.window.is_fullscreen());
        if !visible {
            self.focus_viewer();
        }
        self.header.set_visible(visible);
        self.toolbar.set_visible(visible);
    }

    fn interface_has_input(&self) -> bool {
        self.interface_input.get()
            && gtk::prelude::GtkWindowExt::focus(&self.window).is_some_and(|focus| {
                focus.is_ancestor(&self.toolbar) || focus.is_ancestor(&self.header)
            })
    }

    fn view(&self) -> View {
        let scale = f64::from(self.window.scale_factor());
        let w = self.scroll.width().max(320);
        let h = self.scroll.height().max(240);
        View {
            width: (f64::from(w) * scale * self.zoom.get())
                .clamp(1.0, f64::from(ncat_protocol::MAX_VIEW_DIMENSION)) as u32,
            height: (f64::from(h) * scale * self.zoom.get())
                .clamp(1.0, f64::from(ncat_protocol::MAX_VIEW_DIMENSION))
                as u32,
            quarter_turns: self.quarter_turns.get(),
        }
    }

    fn loading(self: &Rc<Self>) {
        self.status.set_text("읽는 중 · 이전 페이지 표시 중");
        if let Some(timer) = self.loading_timer.borrow_mut().take() {
            timer.remove();
        }
        let weak = Rc::downgrade(self);
        let generation = self.backend.generation();
        *self.loading_timer.borrow_mut() = Some(glib::timeout_add_local_once(
            Duration::from_millis(500),
            move || {
                if let Some(ui) = weak.upgrade() {
                    ui.loading_timer.borrow_mut().take();
                    if !ui.closing.get() && ui.backend.generation() == generation {
                        ui.picture.set_paintable(gdk::Paintable::NONE);
                        ui.status.set_text("페이지를 읽는 중…");
                    }
                }
            },
        ));
    }

    fn open(self: &Rc<Self>, path: PathBuf) {
        self.open_file(gio::File::for_commandline_arg(path), false);
    }

    fn cancel_network(&self) {
        let request = self.network_request.get();
        self.network_request.set(request.wrapping_add(1));
        if let Some(timer) = self.network_timer.borrow_mut().take() {
            timer.remove();
        }
        if let Some(task) = self.network_task.borrow_mut().take() {
            task.abort();
            self.backend
                .diagnose_drop(DropEvent::NetworkCancelled { request });
        }
    }

    fn open_file(self: &Rc<Self>, file: gio::File, from_drop: bool) -> bool {
        self.cancel_network();
        let location = match network::classify(file) {
            Ok(location) => location,
            Err(reason) => {
                self.backend.diagnose_drop(DropEvent::NetworkFailed {
                    request: self.network_request.get(),
                    reason,
                });
                if from_drop && reason == NetworkFailure::UnsupportedScheme {
                    self.backend
                        .diagnose_drop(DropEvent::Rejected(DropRejection::NonLocal));
                }
                self.status.set_text(network::message(reason));
                return false;
            }
        };
        let network::Location::Remote { file, protocol } = location else {
            if let network::Location::Local(path) = location {
                self.open_source(path, from_drop);
            }
            return true;
        };
        self.cancel_reload();
        if let Some(timer) = self.loading_timer.borrow_mut().take() {
            timer.remove();
        }
        if let Some(generation) = self.drop_generation.take() {
            self.backend
                .diagnose_drop(DropEvent::Superseded { generation });
        }
        self.backend.cancel();
        let request = self.network_request.get();
        self.backend
            .diagnose_drop(DropEvent::NetworkResolving { request, protocol });
        self.status
            .set_text("네트워크 폴더 연결 중… 인증이 필요하면 파일 관리자에서 먼저 로그인하세요");
        let parent = self.window.clone();
        let weak = Rc::downgrade(self);
        *self.network_task.borrow_mut() =
            Some(glib::MainContext::default().spawn_local(async move {
                let result = network::resolve(file, parent).await;
                let Some(ui) = weak.upgrade() else {
                    return;
                };
                if ui.closing.get() || ui.network_request.get() != request {
                    return;
                }
                ui.network_task.borrow_mut().take();
                if let Some(timer) = ui.network_timer.borrow_mut().take() {
                    timer.remove();
                }
                match result {
                    Ok((path, bridge)) => {
                        ui.backend
                            .diagnose_drop(DropEvent::NetworkResolved { request, bridge });
                        ui.open_source(path, true);
                    }
                    Err(reason) => {
                        ui.backend
                            .diagnose_drop(DropEvent::NetworkFailed { request, reason });
                        ui.status.set_text(network::message(reason));
                    }
                }
            }));
        let weak = Rc::downgrade(self);
        *self.network_timer.borrow_mut() = Some(glib::timeout_add_local_once(
            Duration::from_secs(60),
            move || {
                if let Some(ui) = weak.upgrade()
                    && ui.network_request.get() == request
                {
                    ui.network_timer.borrow_mut().take();
                    ui.cancel_network();
                    ui.backend.diagnose_drop(DropEvent::NetworkFailed {
                        request,
                        reason: NetworkFailure::Timeout,
                    });
                    ui.status
                        .set_text(network::message(NetworkFailure::Timeout));
                }
            },
        ));
        self.focus_viewer();
        true
    }

    fn open_source(self: &Rc<Self>, path: PathBuf, from_drop: bool) {
        self.cancel_network();
        if let Some(generation) = self.drop_generation.take() {
            self.backend
                .diagnose_drop(DropEvent::Superseded { generation });
        }
        self.cancel_reload();
        self.book.borrow_mut().take();
        self.picture.set_paintable(gdk::Paintable::NONE);
        self.selected.set(0);
        self.zoom.set(1.0);
        self.quarter_turns.set(0);
        self.layout_picture();
        match self.backend.open(path, self.view()) {
            Ok(generation) => {
                if from_drop {
                    self.drop_generation.set(Some(generation));
                    self.backend
                        .diagnose_drop(DropEvent::OpenQueued { generation });
                }
                self.loading();
            }
            Err(code) => {
                self.status.set_text(&code.to_string());
                if from_drop {
                    self.backend.diagnose_drop(DropEvent::OpenFailed {
                        generation: self.backend.generation(),
                        code,
                    });
                }
            }
        }
        self.update_navigation();
        self.focus_viewer();
    }

    fn cancel_reload(&self) {
        if let Some(timer) = self.reload_timer.borrow_mut().take() {
            timer.remove();
        }
    }

    fn load(self: &Rc<Self>, index: usize) {
        self.cancel_network();
        self.cancel_reload();
        let book = self.book.borrow().clone();
        if let Some(book) = book {
            if !book.complete || index >= book.pages.len() {
                return;
            }
            self.selected.set(index);
            if self.backend.load(book, index, self.view()).is_ok() {
                self.loading();
            }
            self.update_navigation();
        }
    }

    fn navigate(self: &Rc<Self>, forward: bool) {
        let len = self
            .book
            .borrow()
            .as_ref()
            .filter(|book| book.complete)
            .map_or(0, |book| book.pages.len());
        if len == 0 {
            return;
        }
        let index = if forward {
            self.selected.get().saturating_add(1).min(len - 1)
        } else {
            self.selected.get().saturating_sub(1)
        };
        if index != self.selected.get() {
            self.zoom.set(1.0);
            self.quarter_turns.set(0);
            self.layout_picture();
            self.load(index);
        } else {
            self.status.set_text(if forward {
                "마지막 페이지입니다"
            } else {
                "첫 페이지입니다"
            });
        }
        self.focus_viewer();
    }

    fn update_navigation(&self) {
        self.syncing.set(true);
        let book = self.book.borrow();
        let len = book
            .as_ref()
            .filter(|b| b.complete)
            .map_or(0, |b| b.pages.len());
        self.previous
            .set_sensitive(len > 0 && self.selected.get() > 0);
        self.next
            .set_sensitive(len > 0 && self.selected.get() + 1 < len);
        self.position.set_sensitive(len > 0);
        self.position.set_range(1.0, len.max(1) as f64);
        self.position.set_value((self.selected.get() + 1) as f64);
        if let Some(book) = book.as_ref() {
            let name = book
                .pages
                .get(self.selected.get())
                .map_or("", |p| p.name.as_str());
            let count = if book.complete {
                book.pages.len().to_string()
            } else {
                "분석 중".into()
            };
            self.window.set_title(Some(&format!(
                "{} / {} · {} — nCatImage",
                self.selected.get() + 1,
                count,
                name
            )));
        }
        self.syncing.set(false);
    }

    fn layout_picture(&self) {
        if self.zoom.get() <= 1.0 {
            self.scroll
                .set_policy(gtk::PolicyType::Never, gtk::PolicyType::Never);
            self.picture.set_size_request(-1, -1);
            // Contain also enlarges a texture smaller than the viewer; ScaleDown never does.
            self.picture.set_content_fit(if self.fill.get() {
                gtk::ContentFit::Contain
            } else {
                gtk::ContentFit::ScaleDown
            });
        } else {
            let (w, h) = self.original.get();
            let (area_w, area_h) = (
                self.scroll.width().max(1) as u32,
                self.scroll.height().max(1) as u32,
            );
            // Zoom steps start from whatever size the current mode shows at 100%.
            let (w, h) = if self.fill.get() {
                contain_size(w, h, area_w, area_h)
            } else {
                fit_size(w.max(1), h.max(1), area_w, area_h).unwrap_or((1, 1))
            };
            self.scroll
                .set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
            self.picture.set_size_request(
                (f64::from(w) * self.zoom.get()) as i32,
                (f64::from(h) * self.zoom.get()) as i32,
            );
            self.picture.set_content_fit(gtk::ContentFit::Contain);
        }
    }

    /// Select 맞춤 (`false`) or 채우기 (`true`) and return to 100% of that mode.
    fn set_fill(self: &Rc<Self>, fill: bool) {
        if self.fill_button.is_active() == fill {
            self.change_zoom(0.0);
        } else {
            // The toggled handler applies the mode and resets zoom.
            self.fill_button.set_active(fill);
        }
    }

    fn change_zoom(self: &Rc<Self>, factor: f64) {
        if self.book.borrow().is_none() {
            return;
        }
        self.zoom.set(if factor == 0.0 {
            1.0
        } else {
            (self.zoom.get() * factor).clamp(1.0, 16.0)
        });
        self.layout_picture();
        self.cancel_reload();
        let weak = Rc::downgrade(self);
        *self.reload_timer.borrow_mut() = Some(glib::timeout_add_local_once(
            Duration::from_millis(100),
            move || {
                if let Some(ui) = weak.upgrade() {
                    ui.reload_timer.borrow_mut().take();
                    ui.load(ui.selected.get());
                }
            },
        ));
    }

    fn rotate(self: &Rc<Self>, backwards: bool) {
        self.quarter_turns
            .set((self.quarter_turns.get() + if backwards { 3 } else { 1 }) % 4);
        self.load(self.selected.get());
    }

    fn dialog(self: &Rc<Self>, folder: bool) {
        let dialog = gtk::FileDialog::builder()
            .title(if folder {
                "만화 폴더 열기"
            } else {
                "이미지 또는 ZIP/CBZ 열기"
            })
            .build();
        let weak = Rc::downgrade(self);
        if folder {
            dialog.select_folder(Some(&self.window), gio::Cancellable::NONE, move |result| {
                if let (Some(ui), Ok(file)) = (weak.upgrade(), result) {
                    ui.open_file(file, false);
                }
            });
        } else {
            dialog.open(Some(&self.window), gio::Cancellable::NONE, move |result| {
                if let (Some(ui), Ok(file)) = (weak.upgrade(), result) {
                    ui.open_file(file, false);
                }
            });
        }
    }

    fn network_dialog(self: &Rc<Self>) {
        let dialog = gtk::Window::builder()
            .title("네트워크 파일 또는 폴더 열기")
            .transient_for(&self.window)
            .modal(true)
            .destroy_with_parent(true)
            .build();
        if let Some(app) = self.window.application() {
            dialog.set_application(Some(&app));
        }
        let cancel = gtk::Button::with_label("취소");
        let open = gtk::Button::with_label("열기");
        dialog.set_default_widget(Some(&open));
        let entry = gtk::Entry::builder()
            .placeholder_text("smb://서버/공유/폴더 또는 ftp://… / sftp://…")
            .input_purpose(gtk::InputPurpose::Url)
            .activates_default(true)
            .width_chars(56)
            .build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
        content.set_margin_top(12);
        content.set_margin_bottom(12);
        content.set_margin_start(12);
        content.set_margin_end(12);
        content.append(&entry);
        let hint = gtk::Label::new(Some(
            "인증은 OS 연결 도구가 처리합니다. 비밀번호를 URL에 넣지 마세요. FTP는 암호화되지 않습니다.",
        ));
        hint.set_wrap(true);
        hint.set_max_width_chars(56);
        content.append(&hint);
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.set_halign(gtk::Align::End);
        actions.append(&cancel);
        actions.append(&open);
        content.append(&actions);
        dialog.set_child(Some(&content));
        let weak = Rc::downgrade(self);
        let weak_dialog = dialog.downgrade();
        open.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.open_file(gio::File::for_uri(entry.text().trim()), false);
            }
            if let Some(dialog) = weak_dialog.upgrade() {
                dialog.close();
            }
        });
        let weak_dialog = dialog.downgrade();
        cancel.connect_clicked(move |_| {
            if let Some(dialog) = weak_dialog.upgrade() {
                dialog.close();
            }
        });
        let keys = gtk::EventControllerKey::new();
        let weak_dialog = dialog.downgrade();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape {
                if let Some(dialog) = weak_dialog.upgrade() {
                    dialog.close();
                }
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        dialog.add_controller(keys);
        dialog.present();
    }

    fn consume(self: &Rc<Self>, event: Event) {
        if self.closing.get() || event.generation() != self.backend.generation() {
            return;
        }
        if let Some(generation) = self.drop_generation.get()
            && generation != event.generation()
        {
            self.drop_generation.set(None);
            self.backend
                .diagnose_drop(DropEvent::Superseded { generation });
        }
        match event {
            Event::Catalog { book, .. } => {
                self.selected.set(book.selected);
                *self.book.borrow_mut() = Some(book);
                self.update_navigation();
            }
            Event::Frame { frame, cached, .. } => {
                if let Some(timer) = self.loading_timer.borrow_mut().take() {
                    timer.remove();
                }
                let info = frame.info.clone();
                if !info.valid() {
                    return;
                }
                let bytes = glib::Bytes::from_owned(FrameBytes(frame));
                let texture = gdk::MemoryTexture::new(
                    info.width as i32,
                    info.height as i32,
                    gdk::MemoryFormat::R8g8b8a8,
                    &bytes,
                    info.stride as usize,
                );
                // GBytes owns FrameBytes, so the reservation survives the GUI's local variables.
                self.picture.set_paintable(Some(&texture));
                if let Some(generation) = self.drop_generation.take() {
                    self.backend
                        .diagnose_drop(DropEvent::Displayed { generation });
                }
                self.original
                    .set((info.original_width, info.original_height));
                self.layout_picture();
                self.status.set_text(&format!(
                    "{} · {}×{} · {} · {:.1} MiB 예약 · M1 축소 표시 / ICC 보정 미구현",
                    info.format,
                    info.original_width,
                    info.original_height,
                    if cached { "캐시 적중" } else { "디코딩" },
                    self.backend.budget_used() as f64 / 1_048_576.0
                ));
                if self.smoke {
                    if let Some(remaining) = self.smoke_steps.get() {
                        if remaining > 0 {
                            self.start_navigation_smoke();
                            return;
                        }
                        println!(
                            "navigation_smoke=ok reserved={} stats={:?}",
                            self.backend.budget_used(),
                            self.backend.cache_stats()
                        );
                    }
                    println!("smoke_title={}", self.window.title().unwrap_or_default());
                    self.smoke_ok.set(true);
                    let weak = Rc::downgrade(self);
                    let hold = if self.smoke_steps.get().is_some() {
                        Duration::from_secs(5)
                    } else {
                        Duration::from_millis(150)
                    };
                    glib::timeout_add_local_once(hold, move || {
                        if let Some(ui) = weak.upgrade() {
                            ui.window.close();
                        }
                    });
                }
            }
            Event::Error { code, .. } => {
                if let Some(generation) = self.drop_generation.take() {
                    self.backend
                        .diagnose_drop(DropEvent::OpenFailed { generation, code });
                }
                if let Some(timer) = self.loading_timer.borrow_mut().take() {
                    timer.remove();
                }
                self.picture.set_paintable(gdk::Paintable::NONE);
                self.status.set_text(&code.to_string());
                if self.smoke {
                    self.smoke_ok.set(false);
                    eprintln!("nCatImage: smoke failure: {code:?}");
                    self.window.close();
                }
            }
        }
    }

    // Explicit test mode only; no timer or automatic navigation in normal reading.
    fn start_navigation_smoke(self: &Rc<Self>) {
        if self.smoke_started.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::timeout_add_local(Duration::from_millis(5), move || {
            let Some(ui) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if ui.closing.get() {
                return glib::ControlFlow::Break;
            }
            let remaining = ui.smoke_steps.get().unwrap_or(0);
            if remaining == 0 {
                return glib::ControlFlow::Break;
            }
            let len = ui.book.borrow().as_ref().map_or(0, |book| book.pages.len());
            if len == 0 {
                return glib::ControlFlow::Continue;
            }
            ui.smoke_steps.set(Some(remaining - 1));
            ui.load((ui.selected.get() + 1) % len);
            if remaining == 1 {
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
    }

    fn close(&self) {
        if self.closing.replace(true) {
            return;
        }
        self.cancel_network();
        self.cancel_reload();
        if let Some(timer) = self.loading_timer.borrow_mut().take() {
            timer.remove();
        }
        self.backend.shutdown();
        self.book.borrow_mut().take();
        self.picture.set_paintable(gdk::Paintable::NONE);
    }
}

fn build(
    app: &gtk::Application,
    initial: Option<PathBuf>,
    smoke: bool,
    smoke_empty: bool,
    smoke_navigation: bool,
    smoke_ok: Rc<Cell<bool>>,
) {
    let backend = match ncat_backend::BackendOptions::from_env()
        .and_then(|options| worker_path().and_then(|path| Backend::with_options(path, options)))
    {
        Ok(backend) => backend,
        Err(_) => {
            eprintln!("nCatImage: backend initialization failed");
            app.quit();
            return;
        }
    };
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title(concat!("nCatImage · v", env!("CARGO_PKG_VERSION")))
        .default_width(1000)
        .default_height(800)
        .build();
    let header = gtk::HeaderBar::new();
    let open = gtk::Button::with_label("파일 열기");
    let folder = gtk::Button::with_label("폴더");
    let network = gtk::Button::with_label("네트워크");
    network.set_tooltip_text(Some("SMB / FTP / SFTP 주소 열기 · Ctrl+L"));
    header.pack_start(&open);
    header.pack_start(&folder);
    header.pack_start(&network);
    let direction = gtk::ToggleButton::with_label("우→좌");
    direction.set_tooltip_text(Some("좌우 방향키의 읽기 방향 · D"));
    header.pack_end(&direction);
    let fullscreen = gtk::Button::with_label("전체 화면");
    header.pack_end(&fullscreen);
    window.set_titlebar(Some(&header));

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let picture = gtk::Picture::builder()
        .hexpand(true)
        .vexpand(true)
        .can_shrink(true)
        .focusable(true)
        .build();
    picture.set_alternative_text(Some("현재 만화 페이지"));
    let scroll = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .child(&picture)
        .build();
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Never);
    let toolbar = gtk::Box::new(gtk::Orientation::Vertical, 6);
    toolbar.set_margin_top(6);
    toolbar.set_margin_bottom(8);
    let status = gtk::Label::new(Some(
        "파일/폴더/ZIP을 열거나 드롭하세요 · ↑ ↓ / Space · ` UI 토글 · Esc 종료",
    ));
    status.set_wrap(true);
    status.set_margin_start(12);
    status.set_margin_end(12);
    toolbar.append(&status);
    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    controls.set_halign(gtk::Align::Center);
    let previous = gtk::Button::with_label("이전");
    let next = gtk::Button::with_label("다음");
    let position = gtk::SpinButton::with_range(1.0, 1.0, 1.0);
    position.set_numeric(true);
    position.set_width_chars(6);
    let smaller = gtk::Button::with_label("−");
    let fit = gtk::Button::with_label("맞춤");
    fit.set_tooltip_text(Some("원본 크기까지만 창에 맞춤 · 9"));
    let fill = gtk::ToggleButton::with_label("채우기");
    fill.set_tooltip_text(Some("비율을 유지하며 창 크기까지 확대 · 0"));
    let larger = gtk::Button::with_label("+");
    let rotate = gtk::Button::with_label("회전");
    for widget in [
        previous.upcast_ref::<gtk::Widget>(),
        position.upcast_ref(),
        next.upcast_ref(),
        smaller.upcast_ref(),
        fit.upcast_ref(),
        fill.upcast_ref(),
        larger.upcast_ref(),
        rotate.upcast_ref(),
    ] {
        controls.append(widget);
    }
    toolbar.append(&controls);
    root.append(&toolbar);
    root.append(&scroll);
    window.set_child(Some(&root));
    let ui = Rc::new(Ui {
        window,
        header,
        toolbar,
        interface_visible: Cell::new(true),
        interface_input: Cell::new(false),
        drop_generation: Cell::new(None),
        network_request: Cell::new(0),
        network_task: RefCell::new(None),
        network_timer: RefCell::new(None),
        picture,
        scroll,
        status,
        position,
        previous,
        next,
        direction_button: direction,
        fill_button: fill,
        fill: Cell::new(false),
        backend,
        book: RefCell::new(None),
        selected: Cell::new(0),
        original: Cell::new((1, 1)),
        zoom: Cell::new(1.0),
        quarter_turns: Cell::new(0),
        syncing: Cell::new(false),
        closing: Cell::new(false),
        reload_timer: RefCell::new(None),
        loading_timer: RefCell::new(None),
        smoke,
        smoke_steps: Cell::new(smoke_navigation.then_some(500)),
        smoke_started: Cell::new(false),
        smoke_ok,
    });
    ui.update_navigation();
    {
        let weak = Rc::downgrade(&ui);
        network.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.network_dialog();
            }
        });
    }
    // Only an explicit pointer click opts controls into keyboard input. Merely
    // showing/restoring a window or pressing Tab must not focus the page spin.
    for widget in [
        ui.header.upcast_ref::<gtk::Widget>(),
        ui.toolbar.upcast_ref(),
    ] {
        let click = gtk::GestureClick::new();
        click.set_button(0);
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(&ui);
        click.connect_pressed(move |_, _, _, _| {
            if let Some(ui) = weak.upgrade() {
                ui.interface_input.set(true);
            }
        });
        widget.add_controller(click);
    }
    {
        let click = gtk::GestureClick::new();
        click.set_button(0);
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(&ui);
        click.connect_pressed(move |_, _, _, _| {
            if let Some(ui) = weak.upgrade() {
                ui.focus_viewer();
            }
        });
        ui.scroll.add_controller(click);
    }
    {
        let weak = Rc::downgrade(&ui);
        ui.window.connect_fullscreened_notify(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.update_interface();
            }
        });
    }
    {
        let weak = Rc::downgrade(&ui);
        open.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.dialog(false);
            }
        });
    }
    {
        let weak = Rc::downgrade(&ui);
        folder.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.dialog(true);
            }
        });
    }
    {
        let weak = Rc::downgrade(&ui);
        fullscreen.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                if ui.window.is_fullscreen() {
                    ui.window.unfullscreen();
                } else {
                    ui.window.fullscreen();
                }
            }
        });
    }
    {
        let weak = Rc::downgrade(&ui);
        ui.previous.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.navigate(false);
            }
        });
    }
    {
        let weak = Rc::downgrade(&ui);
        ui.next.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.navigate(true);
            }
        });
    }
    {
        let weak = Rc::downgrade(&ui);
        smaller.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.change_zoom(1.0 / 1.25);
            }
        });
    }
    {
        let weak = Rc::downgrade(&ui);
        larger.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.change_zoom(1.25);
            }
        });
    }
    {
        let weak = Rc::downgrade(&ui);
        fit.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.set_fill(false);
            }
        });
    }
    {
        let weak = Rc::downgrade(&ui);
        ui.fill_button.connect_toggled(move |button| {
            if let Some(ui) = weak.upgrade() {
                ui.fill.set(button.is_active());
                ui.layout_picture();
                ui.change_zoom(0.0);
            }
        });
    }
    {
        let weak = Rc::downgrade(&ui);
        rotate.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.rotate(false);
            }
        });
    }
    {
        let weak = Rc::downgrade(&ui);
        ui.position.connect_value_changed(move |position| {
            if let Some(ui) = weak.upgrade()
                && !ui.syncing.get()
            {
                ui.load(position.value_as_int().saturating_sub(1) as usize);
            }
        });
    }
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let weak = Rc::downgrade(&ui);
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let Some(ui) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let ctrl = modifiers.contains(gdk::ModifierType::CONTROL_MASK);
            if ctrl && matches!(key, gdk::Key::l | gdk::Key::L) {
                ui.network_dialog();
                return glib::Propagation::Stop;
            }
            if ctrl && matches!(key, gdk::Key::o | gdk::Key::O) {
                ui.dialog(modifiers.contains(gdk::ModifierType::SHIFT_MASK));
                return glib::Propagation::Stop;
            }
            if ctrl && matches!(key, gdk::Key::q | gdk::Key::Q) {
                ui.window.close();
                return glib::Propagation::Stop;
            }
            if key == gdk::Key::F11 {
                if ui.window.is_fullscreen() {
                    ui.window.unfullscreen();
                } else {
                    ui.window.fullscreen();
                }
                return glib::Propagation::Stop;
            }
            if key == gdk::Key::Escape {
                ui.window.close();
                return glib::Propagation::Stop;
            }
            if key == gdk::Key::grave {
                if !ui.window.is_fullscreen() {
                    ui.interface_visible.set(!ui.interface_visible.get());
                    ui.update_interface();
                }
                return glib::Propagation::Stop;
            }
            // Explicitly clicked controls keep native text/spin/Tab behavior;
            // Escape, F11 and the interface toggle remain window shortcuts.
            if ui.interface_has_input() {
                return glib::Propagation::Proceed;
            }
            if matches!(key, gdk::Key::Tab | gdk::Key::ISO_Left_Tab) {
                return glib::Propagation::Stop;
            }
            if modifiers.intersects(
                gdk::ModifierType::CONTROL_MASK
                    | gdk::ModifierType::ALT_MASK
                    | gdk::ModifierType::SUPER_MASK,
            ) {
                return glib::Propagation::Proceed;
            }
            ui.focus_viewer();
            if let Some(forward) = page_step(key) {
                ui.navigate(forward);
                return glib::Propagation::Stop;
            }
            match key {
                gdk::Key::Right | gdk::Key::Left => {
                    let direction = if ui.direction_button.is_active() {
                        Direction::RightToLeft
                    } else {
                        Direction::LeftToRight
                    };
                    ui.navigate(physical_next(direction, key == gdk::Key::Right));
                }
                gdk::Key::Home => ui.load(0),
                gdk::Key::End => {
                    let len = ui.book.borrow().as_ref().map_or(0, |b| b.pages.len());
                    if len > 0 {
                        ui.load(len - 1);
                    }
                }
                gdk::Key::plus | gdk::Key::equal => ui.change_zoom(1.25),
                gdk::Key::minus => ui.change_zoom(1.0 / 1.25),
                gdk::Key::_9 | gdk::Key::KP_9 => ui.set_fill(false),
                gdk::Key::_0 | gdk::Key::KP_0 => ui.set_fill(true),
                gdk::Key::r | gdk::Key::R => {
                    ui.rotate(modifiers.contains(gdk::ModifierType::SHIFT_MASK))
                }
                gdk::Key::d => ui
                    .direction_button
                    .set_active(!ui.direction_button.is_active()),
                gdk::Key::F5 => {
                    let source = ui.book.borrow().as_ref().map(|b| b.source.path());
                    if let Some(source) = source {
                        ui.open(source);
                    }
                }
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
    }
    ui.window.add_controller(keys);

    let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    {
        let weak = Rc::downgrade(&ui);
        let accumulated = Cell::new(0.0_f64);
        let last_page = Cell::new(None::<std::time::Instant>);
        wheel.connect_scroll(move |controller, _, dy| {
            let Some(ui) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if controller
                .current_event_state()
                .contains(gdk::ModifierType::CONTROL_MASK)
            {
                if dy != 0.0 {
                    ui.change_zoom(if dy < 0.0 { 1.1 } else { 1.0 / 1.1 });
                }
                return glib::Propagation::Stop;
            }
            if ui.zoom.get() > 1.0 {
                return glib::Propagation::Proceed;
            }
            let old = accumulated.get();
            accumulated.set(if old.signum() != dy.signum() {
                dy
            } else {
                old + dy
            });
            if accumulated.get().abs() >= 1.0
                && last_page
                    .get()
                    .is_none_or(|last| last.elapsed() >= Duration::from_millis(100))
            {
                ui.navigate(accumulated.get() > 0.0);
                accumulated.set(0.0);
                last_page.set(Some(std::time::Instant::now()));
            }
            glib::Propagation::Stop
        });
    }
    ui.picture.add_controller(wheel);

    let drag = gtk::GestureDrag::new();
    let drag_origin = Rc::new(Cell::new((0.0, 0.0)));
    {
        let weak = Rc::downgrade(&ui);
        let origin = drag_origin.clone();
        drag.connect_drag_begin(move |_, _, _| {
            if let Some(ui) = weak.upgrade() {
                origin.set((
                    ui.scroll.hadjustment().value(),
                    ui.scroll.vadjustment().value(),
                ));
                ui.focus_viewer();
            }
        });
    }
    {
        let weak = Rc::downgrade(&ui);
        drag.connect_drag_update(move |_, dx, dy| {
            if let Some(ui) = weak.upgrade()
                && ui.zoom.get() > 1.0
            {
                let (x, y) = drag_origin.get();
                ui.scroll.hadjustment().set_value(x - dx);
                ui.scroll.vadjustment().set_value(y - dy);
            }
        });
    }
    ui.picture.add_controller(drag);

    // Prefer the file-list representation so count/locality can be validated,
    // while retaining GFile compatibility for sources offering a single file.
    let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    drop.set_types(&[gdk::FileList::static_type(), gio::File::static_type()]);
    drop.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let weak = Rc::downgrade(&ui);
        drop.connect_accept(move |_, offered| {
            let formats = offered.formats().union_deserialize_types();
            let compatible = formats.contains_type(gdk::FileList::static_type())
                || formats.contains_type(gio::File::static_type());
            if let Some(ui) = weak.upgrade() {
                ui.backend.diagnose_drop(DropEvent::Accepted { compatible });
            }
            compatible
        });
    }
    {
        let weak = Rc::downgrade(&ui);
        drop.connect_drop(move |_, value, _, _| {
            let Some(ui) = weak.upgrade() else {
                return false;
            };
            let (files, file_list) = if let Ok(list) = value.get::<gdk::FileList>() {
                (list.files(), true)
            } else if let Ok(file) = value.get::<gio::File>() {
                (vec![file], false)
            } else {
                ui.backend
                    .diagnose_drop(DropEvent::Rejected(DropRejection::UnsupportedValue));
                ui.status.set_text("지원하지 않는 드롭 형식입니다");
                return false;
            };
            ui.backend.diagnose_drop(DropEvent::Received {
                file_list,
                items: files.len(),
            });
            match single_drop_file(&files) {
                Ok(file) => ui.open_file(file.clone(), true),
                Err(reason) => {
                    ui.backend.diagnose_drop(DropEvent::Rejected(reason));
                    ui.status.set_text(match reason {
                        DropRejection::Empty => "드롭된 파일이 없습니다",
                        DropRejection::MultipleItems => "파일 또는 폴더를 한 개씩 드롭하세요",
                        DropRejection::NonLocal => "로컬 또는 SMB·FTP·SFTP 위치만 열 수 있습니다",
                        DropRejection::UnsupportedValue => "지원하지 않는 드롭 형식입니다",
                    });
                    false
                }
            }
        });
    }
    ui.window.add_controller(drop);
    // Observe even unsupported offers, before data negotiation. Do not log
    // motion events, URI strings, file names, paths or arbitrary MIME strings.
    let drag_observer = gtk::DropControllerMotion::new();
    drag_observer.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let weak = Rc::downgrade(&ui);
        drag_observer.connect_enter(move |controller, _, _| {
            if let Some(ui) = weak.upgrade()
                && let Some(offered) = controller.drop()
            {
                let formats = offered.formats();
                ui.backend.diagnose_drop(DropEvent::Entered {
                    file_list: formats.contains_type(gdk::FileList::static_type()),
                    file: formats.contains_type(gio::File::static_type()),
                    uri_list: formats.contain_mime_type("text/uri-list"),
                    portal: formats.contain_mime_type("application/vnd.portal.filetransfer")
                        || formats.contain_mime_type("application/vnd.portal.files"),
                    actions: offered.actions().bits(),
                    wayland: ui.picture.display().type_().name().contains("Wayland"),
                });
            }
        });
    }
    {
        let weak = Rc::downgrade(&ui);
        drag_observer.connect_leave(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.backend.diagnose_drop(DropEvent::Left);
            }
        });
    }
    ui.window.add_controller(drag_observer);
    ui.backend.diagnose_drop(DropEvent::Ready {
        wayland: ui.picture.display().type_().name().contains("Wayland"),
    });
    {
        let weak = Rc::downgrade(&ui);
        ui.window.connect_close_request(move |_| {
            if let Some(ui) = weak.upgrade() {
                if ui.closing.get() {
                    return glib::Propagation::Stop;
                }
                ui.close();
                let started = std::time::Instant::now();
                // Bounded shutdown-only check, not a steady-state result polling loop.
                // Keep Ui alive until the coordinator has killed/reaped the worker.
                glib::timeout_add_local(Duration::from_millis(10), move || {
                    if ui.backend.is_closed() || started.elapsed() >= Duration::from_millis(1500) {
                        ui.window.destroy();
                        glib::ControlFlow::Break
                    } else {
                        glib::ControlFlow::Continue
                    }
                });
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
    }
    // Awaiting this bounded channel wakes the main context; no idle timer/poll loop.
    let events = ui.backend.events();
    let receiver_ui = ui.clone();
    glib::MainContext::default().spawn_local(async move {
        while let Ok(event) = events.recv().await {
            receiver_ui.consume(event);
            // Yield between results so queued frames cannot upload in one UI dispatch.
            glib::timeout_future(Duration::from_millis(1)).await;
        }
    });
    ui.window.present();
    ui.focus_viewer();
    if let Some(path) = initial {
        let ui = ui.clone();
        glib::idle_add_local_once(move || ui.open(path));
    }
    if smoke {
        if smoke_empty {
            let weak = Rc::downgrade(&ui);
            glib::timeout_add_local_once(Duration::from_millis(500), move || {
                if let Some(ui) = weak.upgrade() {
                    ui.smoke_ok.set(ui.window.is_mapped());
                    ui.window.close();
                }
            });
        }
        let weak = Rc::downgrade(&ui);
        glib::timeout_add_local_once(
            Duration::from_secs(if smoke_navigation { 60 } else { 10 }),
            move || {
                if let Some(ui) = weak.upgrade() {
                    ui.window.close();
                }
            },
        );
    }
}

fn main() -> glib::ExitCode {
    std::panic::set_hook(Box::new(|_| eprintln!("nCatImage: internal failure")));
    let mut args = std::env::args_os().skip(1);
    let first = args.next();
    let smoke_empty = first.as_ref().is_some_and(|value| value == "--smoke-empty");
    let smoke_navigation = first
        .as_ref()
        .is_some_and(|value| value == "--smoke-navigation");
    let smoke = smoke_empty
        || smoke_navigation
        || first.as_ref().is_some_and(|value| value == "--smoke-test");
    let initial = if smoke_empty {
        None
    } else if smoke {
        args.next()
    } else {
        first
    }
    .map(PathBuf::from);
    let smoke_ok = Rc::new(Cell::new(false));
    let app = gtk::Application::builder()
        .application_id("io.github.ncatimage.Development")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    {
        let smoke_ok = smoke_ok.clone();
        app.connect_activate(move |app| {
            build(
                app,
                initial.clone(),
                smoke,
                smoke_empty,
                smoke_navigation,
                smoke_ok.clone(),
            )
        });
    }
    let result = app.run_with_args::<&str>(&[]);
    if smoke && !smoke_ok.get() {
        glib::ExitCode::FAILURE
    } else {
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_accepts_one_local_file_or_folder_without_inspecting_the_filesystem() {
        let file = gio::File::for_path("/not-read/book with spaces.cbz");
        assert_eq!(
            single_drop_file(&[file]).unwrap().path().unwrap(),
            PathBuf::from("/not-read/book with spaces.cbz")
        );
        let folder = gio::File::for_uri("file:///not-read/%ED%8F%B4%EB%8D%94");
        assert_eq!(
            single_drop_file(&[folder]).unwrap().path().unwrap(),
            PathBuf::from("/not-read/폴더")
        );
    }

    #[test]
    fn drop_rejects_empty_multiple_and_retains_remote_uris_for_resolution() {
        assert_eq!(single_drop_file(&[]), Err(DropRejection::Empty));
        let file = gio::File::for_path("/not-read/a.png");
        assert_eq!(
            single_drop_file(&[file.clone(), file]),
            Err(DropRejection::MultipleItems)
        );
        let remote = gio::File::for_uri("smb://server/share/book.cbz");
        assert_eq!(
            single_drop_file(std::slice::from_ref(&remote))
                .unwrap()
                .uri(),
            remote.uri()
        );
    }

    #[test]
    #[cfg(unix)]
    fn drop_preserves_non_utf8_local_paths() {
        use std::os::unix::ffi::OsStringExt;
        let path = PathBuf::from(std::ffi::OsString::from_vec(b"/not-read/\xff.png".to_vec()));
        assert_eq!(
            single_drop_file(&[gio::File::for_path(&path)])
                .unwrap()
                .path()
                .unwrap(),
            path
        );
    }

    #[test]
    fn fullscreen_always_hides_interface() {
        assert!(!interface_shown(true, true));
        assert!(!interface_shown(false, true));
    }

    #[test]
    fn fill_enlarges_small_images_and_keeps_aspect_ratio() {
        // Small portrait page in a landscape viewer: height-limited, enlarged 2x.
        assert_eq!(contain_size(400, 600, 1600, 1200), (800, 1200));
        // Large landscape image: width-limited, reduced.
        assert_eq!(contain_size(4000, 1000, 1600, 1200), (1600, 400));
        // Same aspect ratio fills the viewer exactly.
        assert_eq!(contain_size(800, 600, 1600, 1200), (1600, 1200));
        assert_eq!(contain_size(0, 0, 0, 0), (1, 1));
    }

    #[test]
    fn windowed_interface_respects_toggle() {
        assert!(interface_shown(true, false));
        assert!(!interface_shown(false, false));
    }

    #[test]
    fn vertical_keys_match_page_keys_independent_of_reading_direction() {
        for key in [gdk::Key::Down, gdk::Key::KP_Down, gdk::Key::space] {
            assert_eq!(page_step(key), page_step(gdk::Key::Page_Down));
            assert_eq!(page_step(key), Some(true));
        }
        for key in [gdk::Key::Up, gdk::Key::KP_Up, gdk::Key::BackSpace] {
            assert_eq!(page_step(key), page_step(gdk::Key::Page_Up));
            assert_eq!(page_step(key), Some(false));
        }
        assert_eq!(page_step(gdk::Key::Left), None);
        assert_eq!(page_step(gdk::Key::Escape), None);
    }
}
