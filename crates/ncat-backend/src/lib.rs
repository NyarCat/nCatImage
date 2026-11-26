#![forbid(unsafe_code)]

mod cache;
mod diagnostics;
mod neighbors;
mod options;
mod worker_command;
use cache::{FrameCache, Key, Revision, Target};
pub use diagnostics::{DropEvent, DropRejection, NetworkBridge, NetworkFailure, NetworkProtocol};
pub use options::BackendOptions;
use std::cell::Cell;
pub use worker_command::WorkerCommand;

use ncat_core::{Budget, DEFAULT_BUDGET, MIB, Permit};
use ncat_protocol::{
    Envelope, ErrorCode, FrameInfo, INDEX_BYTES, MAX_ENTRIES, MAX_OUTPUT, Page, PageKey, RawPath,
    Request, Response, read_message, write_message,
};
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub use ncat_protocol::ErrorCode as ViewerError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct View {
    pub width: u32,
    pub height: u32,
    pub quarter_turns: u8,
}
impl Default for View {
    fn default() -> Self {
        Self {
            width: 1200,
            height: 1200,
            quarter_turns: 0,
        }
    }
}

#[derive(Debug)]
pub struct Book {
    pub source: RawPath,
    pub title: String,
    pub pages: Vec<Page>,
    pub selected: usize,
    pub complete: bool,
    index_permit: Arc<Permit>,
    epoch: u64,
    source_revision: Option<Revision>,
}

pub enum Deletion {
    Archive,
    File {
        path: PathBuf,
        name: String,
        next: Option<PathBuf>,
    },
}

impl Book {
    pub fn deletion(&self, index: usize) -> Option<Deletion> {
        let page = self.pages.get(index)?;
        let ncat_protocol::PageKey::File(path) = &page.key else {
            return Some(Deletion::Archive);
        };
        let next = self
            .pages
            .get(index + 1)
            .or_else(|| {
                index
                    .checked_sub(1)
                    .and_then(|previous| self.pages.get(previous))
            })
            .and_then(|page| match &page.key {
                ncat_protocol::PageKey::File(path) => Some(path.path()),
                ncat_protocol::PageKey::Zip(_) => None,
            });
        Some(Deletion::File {
            path: path.path(),
            name: page.name.clone(),
            next,
        })
    }

    /// `None` leaves an unrelated book alone; `Some(None)` clears the last image.
    pub fn after_deletion(
        &self,
        selected: usize,
        removed: &Path,
        next: Option<PathBuf>,
    ) -> Option<Option<PathBuf>> {
        let current = match &self.pages.get(selected)?.key {
            ncat_protocol::PageKey::File(path) => path.path(),
            ncat_protocol::PageKey::Zip(_) => return None,
        };
        if current == removed {
            return Some(next);
        }
        self.pages.iter()
            .any(|page| matches!(&page.key, ncat_protocol::PageKey::File(path) if path.path() == removed))
            .then_some(Some(current))
    }
}

#[derive(Debug)]
pub struct Frame {
    pub info: FrameInfo,
    pixels: Vec<u8>,
    _permit: Permit,
}
impl AsRef<[u8]> for Frame {
    fn as_ref(&self) -> &[u8] {
        &self.pixels
    }
}

/// Keeps the pixel AND estimated GUI/staging budget alive through GBytes/GdkTexture ownership.
pub struct FrameBytes(pub Arc<Frame>);
impl AsRef<[u8]> for FrameBytes {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref().as_ref()
    }
}

#[derive(Debug)]
pub enum Event {
    Neighbor {
        generation: u64,
        request: u64,
        result: Result<Option<PathBuf>, ErrorCode>,
    },
    Catalog {
        generation: u64,
        book: Arc<Book>,
    },
    Frame {
        generation: u64,
        frame: Arc<Frame>,
        cached: bool,
    },
    Error {
        generation: u64,
        code: ErrorCode,
    },
    AnimationEnded {
        generation: u64,
    },
}
impl Event {
    pub fn generation(&self) -> u64 {
        match self {
            Self::Catalog { generation, .. }
            | Self::Neighbor { generation, .. }
            | Self::Frame { generation, .. }
            | Self::Error { generation, .. } => *generation,
            Self::AnimationEnded { generation } => *generation,
        }
    }
}

enum Task {
    Open {
        path: PathBuf,
        view: View,
    },
    Load {
        book: Arc<Book>,
        index: usize,
        view: View,
    },
    Animate {
        book: Arc<Book>,
        index: usize,
        view: View,
        seek: Option<u32>,
    },
}
struct Job {
    generation: u64,
    task: Task,
}
struct Neighbor {
    request: u64,
    book: Arc<Book>,
    forward: bool,
}
struct Shared {
    pending: Mutex<Option<Job>>,
    neighbor: Mutex<Option<Neighbor>>,
    neighbor_request: AtomicU64,
    diagnostics: Mutex<diagnostics::Diagnostics>,
    wake: Condvar,
    generation: AtomicU64,
    closing: AtomicBool,
    finished: AtomicBool,
    budget: Budget,
    stats: Stats,
    prefetch_enabled: AtomicBool,
    options: BackendOptions,
}

impl Shared {
    fn record(&self, event: String) {
        self.diagnostics.lock().unwrap().record(event);
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub decodes: u64,
    pub prefetches: u64,
    pub prefetched_hits: u64,
    pub promotions: u64,
    pub evictions: u64,
    pub bytes: u64,
    pub worker_starts: u64,
    pub source_reopens: u64,
}
#[derive(Default)]
struct Stats {
    hits: AtomicU64,
    misses: AtomicU64,
    decodes: AtomicU64,
    prefetches: AtomicU64,
    prefetched_hits: AtomicU64,
    promotions: AtomicU64,
    evictions: AtomicU64,
    bytes: AtomicU64,
    worker_starts: AtomicU64,
    source_reopens: AtomicU64,
}
impl Stats {
    fn snapshot(&self) -> CacheStats {
        CacheStats {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            decodes: self.decodes.load(Ordering::Relaxed),
            prefetches: self.prefetches.load(Ordering::Relaxed),
            prefetched_hits: self.prefetched_hits.load(Ordering::Relaxed),
            promotions: self.promotions.load(Ordering::Relaxed),
            evictions: self.evictions.load(Ordering::Relaxed),
            bytes: self.bytes.load(Ordering::Acquire),
            worker_starts: self.worker_starts.load(Ordering::Relaxed),
            source_reopens: self.source_reopens.load(Ordering::Relaxed),
        }
    }
}

/// A foreground request may join an already running decode for exactly the same target.
/// Changing book/view/page (or disabling prefetch) still cancels and reaps the worker.
struct WorkGuard {
    generation: u64,
    target: Option<Target>,
    prefetch: bool,
    joined: Cell<bool>,
}
impl WorkGuard {
    fn new(generation: u64, target: Option<Target>, prefetch: bool) -> Self {
        Self {
            generation,
            target,
            prefetch,
            joined: Cell::new(false),
        }
    }
    fn allowed_locked(&self, shared: &Shared, pending: &Option<Job>) -> bool {
        if shared.closing.load(Ordering::Acquire) {
            return false;
        }
        if self.prefetch && !shared.prefetch_enabled.load(Ordering::Acquire) {
            return false;
        }
        if current(shared, self.generation) {
            return true;
        }
        let joined = match (&self.target, pending) {
            (
                Some(target),
                Some(Job {
                    generation,
                    task: Task::Load { book, index, view },
                }),
            ) if *generation == shared.generation.load(Ordering::Acquire) => {
                *target == Target::new(book, *index, *view)
            }
            _ => false,
        };
        if joined && !self.joined.replace(true) && self.prefetch {
            shared.stats.promotions.fetch_add(1, Ordering::Relaxed);
        }
        joined
    }
    fn allowed(&self, shared: &Shared) -> bool {
        let pending = shared.pending.lock().unwrap();
        self.allowed_locked(shared, &pending)
    }
}

#[derive(Clone)]
pub struct Backend {
    shared: Arc<Shared>,
    events: async_channel::Receiver<Event>,
}

impl Backend {
    pub fn start(worker: PathBuf) -> io::Result<Self> {
        Self::with_budget(worker, DEFAULT_BUDGET)
    }
    pub fn with_budget(worker: PathBuf, limit: u64) -> io::Result<Self> {
        Self::with_options(
            worker,
            BackendOptions {
                budget: limit,
                cache_bytes: (limit / 8).min(48 * MIB),
                ..BackendOptions::default()
            },
        )
    }
    pub fn with_options(worker: PathBuf, options: BackendOptions) -> io::Result<Self> {
        Self::with_worker_command(WorkerCommand::separate(worker), options)
    }

    pub fn with_worker_command(worker: WorkerCommand, options: BackendOptions) -> io::Result<Self> {
        let options = options.validate()?;
        let shared = Arc::new(Shared {
            pending: Mutex::new(None),
            neighbor: Mutex::new(None),
            neighbor_request: AtomicU64::new(0),
            diagnostics: Mutex::new(diagnostics::Diagnostics::new()),
            wake: Condvar::new(),
            generation: AtomicU64::new(0),
            closing: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            budget: Budget::new(options.budget),
            stats: Stats::default(),
            prefetch_enabled: AtomicBool::new(options.prefetch),
            options,
        });
        shared.record("stage=backend_ready".into());
        let (sender, events) = async_channel::bounded(4);
        let control = shared.clone();
        thread::Builder::new()
            .name("ncat-coordinator".into())
            .spawn(move || coordinator(worker, control, sender, options.cache_bytes))?;
        Ok(Self { shared, events })
    }
    fn submit(&self, task: Task) -> Result<u64, ErrorCode> {
        let mut pending = self.shared.pending.lock().unwrap();
        if self.shared.closing.load(Ordering::Acquire) {
            return Err(ErrorCode::Cancelled);
        }
        let generation = self.shared.generation.fetch_add(1, Ordering::AcqRel) + 1;
        *pending = Some(Job { generation, task });
        self.shared.wake.notify_one();
        Ok(generation)
    }
    pub fn open(&self, path: impl Into<PathBuf>, view: View) -> Result<u64, ErrorCode> {
        self.cancel_neighbor();
        self.submit(Task::Open {
            path: path.into(),
            view,
        })
    }
    pub fn find_neighbor(&self, book: Arc<Book>, forward: bool) {
        let _pending = self.shared.pending.lock().unwrap();
        let request = self.shared.neighbor_request.fetch_add(1, Ordering::AcqRel) + 1;
        *self.shared.neighbor.lock().unwrap() = Some(Neighbor {
            request,
            book,
            forward,
        });
        self.shared.wake.notify_one();
    }

    pub fn neighbor_is_current(&self, request: u64) -> bool {
        self.shared.neighbor_request.load(Ordering::Acquire) == request
    }
    pub fn accepts(&self, event: &Event) -> bool {
        match event {
            Event::Neighbor { request, .. } => self.neighbor_is_current(*request),
            _ => event.generation() == self.generation(),
        }
    }

    pub fn cancel_neighbor(&self) {
        let mut neighbor = self.shared.neighbor.lock().unwrap();
        self.shared.neighbor_request.fetch_add(1, Ordering::AcqRel);
        *neighbor = None;
    }
    pub fn load(&self, book: Arc<Book>, index: usize, view: View) -> Result<u64, ErrorCode> {
        if index >= book.pages.len() {
            return Err(ErrorCode::Protocol);
        }
        self.submit(Task::Load { book, index, view })
    }
    /// Request one sequential frame, after the frontend's one-shot delay expires.
    pub fn animation_next(
        &self,
        book: Arc<Book>,
        index: usize,
        view: View,
    ) -> Result<u64, ErrorCode> {
        if index >= book.pages.len() {
            return Err(ErrorCode::Protocol);
        }
        self.submit(Task::Animate {
            book,
            index,
            view,
            seek: None,
        })
    }
    pub fn animation_seek(
        &self,
        book: Arc<Book>,
        index: usize,
        view: View,
        frame: u32,
    ) -> Result<u64, ErrorCode> {
        if index >= book.pages.len() || frame >= ncat_protocol::MAX_ANIMATION_FRAMES {
            return Err(ErrorCode::Protocol);
        }
        self.submit(Task::Animate {
            book,
            index,
            view,
            seek: Some(frame),
        })
    }
    pub fn generation(&self) -> u64 {
        self.shared.generation.load(Ordering::Acquire)
    }
    /// Invalidate work while the GUI asynchronously resolves a new source.
    pub fn cancel(&self) {
        self.cancel_neighbor();
        let mut pending = self.shared.pending.lock().unwrap();
        if !self.shared.closing.load(Ordering::Acquire) {
            self.shared.generation.fetch_add(1, Ordering::AcqRel);
            *pending = None;
            self.shared.wake.notify_one();
        }
    }
    /// Record a bounded, typed diagnostic event in memory. No source data is accepted.
    pub fn diagnose_drop(&self, event: DropEvent) {
        if !self.shared.closing.load(Ordering::Acquire) {
            self.shared.record(format!("stage=drop event={event:?}"));
        }
    }
    /// Return the recent redacted diagnostics for this run, oldest first.
    pub fn diagnostic_log(&self) -> String {
        self.shared.diagnostics.lock().unwrap().snapshot()
    }
    pub fn events(&self) -> async_channel::Receiver<Event> {
        self.events.clone()
    }
    pub fn budget_used(&self) -> u64 {
        self.shared.budget.used()
    }
    pub fn cache_stats(&self) -> CacheStats {
        self.shared.stats.snapshot()
    }
    pub fn set_prefetch_enabled(&self, enabled: bool) {
        let _pending = self.shared.pending.lock().unwrap();
        self.shared
            .prefetch_enabled
            .store(enabled, Ordering::Release);
        self.shared.wake.notify_one();
    }
    pub fn shutdown(&self) {
        let mut pending = self.shared.pending.lock().unwrap();
        self.shared.closing.store(true, Ordering::Release);
        self.shared.generation.fetch_add(1, Ordering::AcqRel);
        *pending = None;
        drop(pending);
        self.events.close();
        while self.events.try_recv().is_ok() {}
        self.shared.wake.notify_all();
    }
    pub fn is_closed(&self) -> bool {
        self.shared.finished.load(Ordering::Acquire)
    }
}

fn current(shared: &Shared, generation: u64) -> bool {
    !shared.closing.load(Ordering::Acquire)
        && shared.generation.load(Ordering::Acquire) == generation
}

fn emit(
    sender: &async_channel::Sender<Event>,
    shared: &Shared,
    event: Event,
) -> Result<(), ErrorCode> {
    if !current(shared, event.generation()) {
        return Err(ErrorCode::Cancelled);
    }
    sender
        .send_blocking(event)
        .map_err(|_| ErrorCode::Cancelled)
}

struct Worker {
    protection: Option<ncat_platform::WorkerProtection>,
    animation_workspace: Option<Permit>,
    animation_target: Option<Key>,
    animation_generation: u64,
    child: Option<Child>,
    input: Option<BufWriter<ChildStdin>>,
    responses: mpsc::Receiver<io::Result<Envelope<Response>>>,
    reader: Option<JoinHandle<()>>,
    stop_reader: Arc<AtomicBool>,
    next_id: u64,
    source: Option<RawPath>,
    index_permit: Option<Arc<Permit>>,
}

impl Worker {
    fn spawn(worker: &WorkerCommand) -> Result<Self, ErrorCode> {
        let mut child = worker
            .command()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| ErrorCode::Worker)?;
        let protection = match ncat_platform::start_protected_worker(&mut child) {
            Ok(protection) => protection,
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ErrorCode::Worker);
            }
        };
        let input = BufWriter::new(child.stdin.take().ok_or(ErrorCode::Worker)?);
        let stdout = child.stdout.take().ok_or(ErrorCode::Worker)?;
        let (tx, responses) = mpsc::sync_channel(2);
        let stop_reader = Arc::new(AtomicBool::new(false));
        let stop = stop_reader.clone();
        let reader = match thread::Builder::new()
            .name("ncat-ipc-reader".into())
            .spawn(move || {
                let mut stdout = BufReader::new(stdout);
                loop {
                    let (mut response, terminal) = match read_message::<Response>(&mut stdout) {
                        Ok(Some(response)) => (Ok(response), false),
                        Ok(None) => (
                            Err(io::Error::new(io::ErrorKind::UnexpectedEof, "worker exit")),
                            true,
                        ),
                        Err(e) => (Err(e), true),
                    };
                    loop {
                        if stop.load(Ordering::Acquire) {
                            return;
                        }
                        match tx.try_send(response) {
                            Ok(()) => break,
                            Err(mpsc::TrySendError::Full(value)) => {
                                response = value;
                                thread::park_timeout(Duration::from_millis(2));
                            }
                            Err(mpsc::TrySendError::Disconnected(_)) => return,
                        }
                    }
                    if terminal {
                        return;
                    }
                }
            }) {
            Ok(thread) => thread,
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ErrorCode::Worker);
            }
        };
        Ok(Self {
            protection: Some(protection),
            animation_workspace: None,
            animation_target: None,
            animation_generation: 0,
            child: Some(child),
            input: Some(input),
            responses,
            reader: Some(reader),
            stop_reader,
            next_id: 0,
            source: None,
            index_permit: None,
        })
    }

    fn stop(&mut self) {
        self.stop_reader.store(true, Ordering::Release);
        self.input.take();
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        self.source = None;
        self.index_permit = None;
        self.animation_workspace = None;
        self.animation_target = None;
        self.protection = None;
    }

    fn send(&mut self, request: Request) -> Result<u64, ErrorCode> {
        self.next_id = self.next_id.checked_add(1).ok_or(ErrorCode::Protocol)?;
        write_message(
            self.input.as_mut().ok_or(ErrorCode::Worker)?,
            &Envelope::new(self.next_id, request),
        )
        .map_err(|_| ErrorCode::Worker)?;
        Ok(self.next_id)
    }

    fn receive(
        &mut self,
        id: u64,
        shared: &Shared,
        generation: u64,
        deadline: Instant,
    ) -> Result<Response, ErrorCode> {
        self.receive_guard(
            id,
            shared,
            &WorkGuard::new(generation, None, false),
            deadline,
        )
    }

    fn receive_guard(
        &mut self,
        id: u64,
        shared: &Shared,
        guard: &WorkGuard,
        deadline: Instant,
    ) -> Result<Response, ErrorCode> {
        loop {
            if !guard.allowed(shared) {
                // A worker task cannot be interrupted by dropping its caller. Kill/reap BEFORE permits drop.
                self.stop();
                return Err(ErrorCode::Cancelled);
            }
            if Instant::now() >= deadline {
                self.stop();
                return Err(ErrorCode::Timeout);
            }
            match self.responses.recv_timeout(Duration::from_millis(10)) {
                Ok(Ok(message)) if message.id == id => {
                    return match message.body {
                        Response::Error(e) => Err(e),
                        other => Ok(other),
                    };
                }
                Ok(Ok(_)) => {
                    self.stop();
                    return Err(ErrorCode::Protocol);
                }
                Ok(Err(error)) => {
                    self.stop();
                    return Err(
                        if matches!(
                            error.kind(),
                            io::ErrorKind::UnexpectedEof | io::ErrorKind::BrokenPipe
                        ) {
                            ErrorCode::Worker
                        } else {
                            ErrorCode::Protocol
                        },
                    );
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.stop();
                    return Err(ErrorCode::Worker);
                }
            }
        }
    }

    fn catalog_response(
        &mut self,
        id: u64,
        shared: &Shared,
        generation: u64,
        deadline: Instant,
    ) -> Result<(Vec<Page>, usize), ErrorCode> {
        let mut pages = Vec::new();
        let mut total = 0_u64;
        loop {
            match self.receive(id, shared, generation, deadline)? {
                Response::Pages {
                    pages: batch,
                    selected,
                    done,
                } => {
                    for page in batch {
                        if page.name.len() > ncat_protocol::MAX_LABEL_BYTES
                            || matches!(&page.key,ncat_protocol::PageKey::File(path) if path.0.len() > ncat_protocol::MAX_PATH_BYTES)
                        {
                            return Err(ErrorCode::Protocol);
                        }
                        total = total.checked_add(page.cost()).ok_or(ErrorCode::Limit)?;
                        if total > INDEX_BYTES / 2 || pages.len() >= MAX_ENTRIES {
                            return Err(ErrorCode::Limit);
                        }
                        pages.push(page);
                    }
                    if done {
                        if !pages.is_empty() && selected >= pages.len() {
                            return Err(ErrorCode::Protocol);
                        }
                        return Ok((pages, selected));
                    }
                }
                _ => return Err(ErrorCode::Protocol),
            }
        }
    }

    fn open(
        &mut self,
        source: &RawPath,
        shared: &Shared,
        generation: u64,
        permit: Arc<Permit>,
    ) -> Result<Arc<Book>, ErrorCode> {
        let before = Revision::read(&source.path()).ok();
        let id = self.send(Request::Open {
            source: source.clone(),
        })?;
        let deadline = Instant::now() + shared.options.task_timeout;
        let (title, deferred) = match self.receive(id, shared, generation, deadline)? {
            Response::Opened { title, deferred }
                if title.len() <= ncat_protocol::MAX_LABEL_BYTES =>
            {
                (title, deferred)
            }
            _ => return Err(ErrorCode::Protocol),
        };
        let (pages, selected) = self.catalog_response(id, shared, generation, deadline)?;
        if before != Revision::read(&source.path()).ok() {
            return Err(ErrorCode::Changed);
        }
        self.source = Some(source.clone());
        self.index_permit = Some(permit.clone());
        Ok(Arc::new(Book {
            source: source.clone(),
            title,
            pages,
            selected,
            complete: !deferred,
            index_permit: permit,
            epoch: generation,
            source_revision: before,
        }))
    }

    fn full_catalog(
        &mut self,
        book: &Arc<Book>,
        shared: &Shared,
        generation: u64,
    ) -> Result<Arc<Book>, ErrorCode> {
        let id = self.send(Request::Catalog)?;
        let (pages, selected) = self.catalog_response(
            id,
            shared,
            generation,
            Instant::now() + shared.options.task_timeout,
        )?;
        Ok(Arc::new(Book {
            source: book.source.clone(),
            title: book.title.clone(),
            pages,
            selected,
            complete: true,
            index_permit: book.index_permit.clone(),
            epoch: book.epoch,
            source_revision: book.source_revision.clone(),
        }))
    }

    fn decode(
        &mut self,
        page: &Page,
        view: View,
        shared: &Shared,
        guard: &WorkGuard,
    ) -> Result<Arc<Frame>, ErrorCode> {
        self.decode_frame(page, view, shared, guard, None)?
            .ok_or(ErrorCode::Protocol)
    }

    fn decode_frame(
        &mut self,
        page: &Page,
        view: View,
        shared: &Shared,
        guard: &WorkGuard,
        advance: Option<Option<u32>>,
    ) -> Result<Option<Arc<Frame>>, ErrorCode> {
        if !guard.allowed(shared) {
            return Err(ErrorCode::Cancelled);
        }
        let mut display = shared
            .budget
            .reserve(
                MAX_OUTPUT * ncat_protocol::PRESENTATION_COPIES
                    + ncat_protocol::MAX_METADATA_BYTES as u64
                        * ncat_protocol::METADATA_PRESENTATION_COPIES,
            )
            .ok_or(ErrorCode::Limit)?;
        let remaining = shared.budget.limit().saturating_sub(shared.budget.used());
        let workspace = if advance.is_some() {
            self.animation_workspace
                .as_ref()
                .ok_or(ErrorCode::Protocol)?
                .bytes()
        } else {
            remaining.min(shared.options.workspace_bytes)
        };
        if workspace < ncat_protocol::DECODE_OVERHEAD {
            return Err(ErrorCode::Limit);
        }
        let mut work = if advance.is_some() {
            None
        } else {
            Some(shared.budget.reserve(workspace).ok_or(ErrorCode::Limit)?)
        };
        let directory = tempfile::Builder::new()
            .prefix("ncat-frame-")
            .tempdir()
            .map_err(|_| ErrorCode::Io)?;
        let private = directory.path().join("private");
        ncat_platform::create_private_directory(&private).map_err(|_| ErrorCode::Io)?;
        let mut output = tempfile::NamedTempFile::new_in(&private).map_err(|_| ErrorCode::Io)?;
        shared.stats.decodes.fetch_add(1, Ordering::Relaxed);
        let request = if let Some(seek) = advance {
            match seek {
                Some(index) => Request::AnimationSeek {
                    output: RawPath::new(output.path()),
                    index,
                },
                None => Request::AnimationNext {
                    output: RawPath::new(output.path()),
                },
            }
        } else {
            Request::Decode {
                key: page.key.clone(),
                output: RawPath::new(output.path()),
                max_width: view.width,
                max_height: view.height,
                workspace,
                quarter_turns: view.quarter_turns,
            }
        };
        let id = self.send(request)?;
        let info = match self.receive_guard(
            id,
            shared,
            guard,
            Instant::now() + shared.options.task_timeout,
        )? {
            Response::Frame(info) if info.valid() => info,
            Response::AnimationEnded if advance.is_some() => return Ok(None),
            _ => {
                self.stop();
                return Err(ErrorCode::Protocol);
            }
        };
        if let Some(animation) = &info.animation {
            if animation.workspace_bytes > workspace {
                self.stop();
                return Err(ErrorCode::Protocol);
            }
            if let Some(mut reservation) = work.take() {
                reservation.shrink_to(animation.workspace_bytes);
                self.animation_workspace = Some(reservation);
            }
        } else if advance.is_some() {
            self.stop();
            return Err(ErrorCode::Protocol);
        }
        // Read from the parent's original FD, never a worker-controlled response path.
        let file: &mut File = output.as_file_mut();
        if file.metadata().map_err(|_| ErrorCode::Io)?.len() != info.bytes {
            self.stop();
            return Err(ErrorCode::Protocol);
        }
        file.seek(SeekFrom::Start(0)).map_err(|_| ErrorCode::Io)?;
        let size = usize::try_from(info.bytes).map_err(|_| ErrorCode::Limit)?;
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(size)
            .map_err(|_| ErrorCode::Limit)?;
        pixels.resize(size, 0);
        file.read_exact(&mut pixels).map_err(|_| ErrorCode::Io)?;
        // The GUI never borrows memory writable by the worker. The extra copy is budgeted.
        if !guard.allowed(shared) {
            return Err(ErrorCode::Cancelled);
        }
        display.shrink_to(
            info.bytes * ncat_protocol::PRESENTATION_COPIES
                + info.metadata_bytes() * ncat_protocol::METADATA_PRESENTATION_COPIES,
        );
        Ok(Some(Arc::new(Frame {
            info,
            pixels,
            _permit: display,
        })))
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Unwind-only safety net; normal paths call stop() on the coordinator, never the UI.
        self.stop_reader.store(true, Ordering::Release);
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct PrefetchPlan {
    book: Arc<Book>,
    index: usize,
    view: View,
    generation: u64,
}

struct Engine {
    path: WorkerCommand,
    worker: Option<Worker>,
    cache: FrameCache,
    previous: Option<(u64, usize)>,
    forward: bool,
}

impl Engine {
    fn trace(&self, stage: &str, shared: &Shared) {
        shared.record(format!(
            "stage={stage} generation={} reserved={} budget={} stats={:?}",
            shared.generation.load(Ordering::Acquire),
            shared.budget.used(),
            shared.budget.limit(),
            shared.stats.snapshot()
        ));
    }
    fn worker(&mut self, shared: &Shared) -> Result<&mut Worker, ErrorCode> {
        if self.worker.as_ref().is_some_and(|w| w.child.is_none()) {
            self.stop_worker();
        }
        if self.worker.is_none() {
            self.worker = Some(Worker::spawn(&self.path)?);
            shared.stats.worker_starts.fetch_add(1, Ordering::Relaxed);
            self.trace("worker_started", shared);
        }
        self.worker.as_mut().ok_or(ErrorCode::Worker)
    }
    fn stop_worker(&mut self) {
        if let Some(mut worker) = self.worker.take() {
            worker.stop();
        }
    }
    fn publish_cache_stats(&self, shared: &Shared) {
        shared
            .stats
            .evictions
            .store(self.cache.evictions(), Ordering::Relaxed);
        shared
            .stats
            .bytes
            .store(self.cache.bytes(), Ordering::Release);
    }

    fn key(
        &mut self,
        book: &Book,
        index: usize,
        view: View,
        shared: &Shared,
    ) -> Result<Key, ErrorCode> {
        match Key::read(book, index, view) {
            Ok(key) => {
                self.cache.discard_old_revisions(&key);
                self.publish_cache_stats(shared);
                Ok(key)
            }
            Err(error) => {
                shared.record(format!(
                    "stage=key_validation_failed error={error:?} epoch={} view={}x{} rotation={}",
                    book.epoch, view.width, view.height, view.quarter_turns
                ));
                if let (Some(before), Ok(now)) =
                    (&book.source_revision, Revision::read(&book.source.path()))
                {
                    shared.record(before.differences(&now));
                }
                self.trace("key_validation_failure", shared);
                if matches!(book.pages[index].key, ncat_protocol::PageKey::Zip(_)) {
                    self.cache.clear();
                } else {
                    self.cache.remove_target(&Target::new(book, index, view));
                }
                self.publish_cache_stats(shared);
                Err(error)
            }
        }
    }

    fn frame(
        &mut self,
        book: &Arc<Book>,
        index: usize,
        view: View,
        shared: &Shared,
        guard: &WorkGuard,
    ) -> Result<(Arc<Frame>, bool), ErrorCode> {
        if !guard.allowed(shared) {
            return Err(ErrorCode::Cancelled);
        }
        if self
            .worker
            .as_ref()
            .is_some_and(|w| w.animation_workspace.is_some())
        {
            self.stop_worker();
        }
        shared.record(format!("stage=frame_request request_generation={} epoch={} background={} view={}x{} rotation={}", guard.generation, book.epoch, guard.prefetch, view.width, view.height, view.quarter_turns));
        let key = self.key(book, index, view, shared)?;
        if let Some((frame, prefetched)) = self.cache.get(&key) {
            self.trace("cache_hit", shared);
            if !guard.prefetch {
                shared.stats.hits.fetch_add(1, Ordering::Relaxed);
                if prefetched {
                    shared.stats.prefetched_hits.fetch_add(1, Ordering::Relaxed);
                }
            }
            return Ok((frame, true));
        }
        if !guard.prefetch {
            shared.stats.misses.fetch_add(1, Ordering::Relaxed);
        }
        self.trace(
            if guard.prefetch {
                "prefetch_decode"
            } else {
                "foreground_decode"
            },
            shared,
        );
        if self.worker(shared)?.source.as_ref() != Some(&book.source) {
            let worker = self.worker(shared)?;
            let opened = worker.open(
                &book.source,
                shared,
                guard.generation,
                book.index_permit.clone(),
            )?;
            if !opened.complete {
                worker.full_catalog(&opened, shared, guard.generation)?;
            }
        }
        // A cache may use spare memory but must never make a foreground decode fail.
        // Typed admission failures occur before full decode; retry once without optional caches.
        let frame = match self
            .worker(shared)?
            .decode(&book.pages[index], view, shared, guard)
        {
            Err(ErrorCode::Changed) if !guard.prefetch => {
                self.trace("worker_reported_changed", shared);
                // A worker session may have gone stale. Never treat its error alone as
                // proof that the user's source changed, and never mask a real change.
                // Revalidate the parent's original key, then rebuild the session once.
                if !guard.allowed(shared) {
                    return Err(ErrorCode::Cancelled);
                }
                if self.key(book, index, view, shared)? != key {
                    return Err(ErrorCode::Changed);
                }
                self.stop_worker();
                self.trace("reopening_unchanged_source", shared);
                shared.stats.source_reopens.fetch_add(1, Ordering::Relaxed);
                let worker = self.worker(shared)?;
                let opened = worker.open(
                    &book.source,
                    shared,
                    guard.generation,
                    book.index_permit.clone(),
                )?;
                if !opened.complete {
                    worker.full_catalog(&opened, shared, guard.generation)?;
                }
                if self.key(book, index, view, shared)? != key {
                    return Err(ErrorCode::Changed);
                }
                match self
                    .worker(shared)?
                    .decode(&book.pages[index], view, shared, guard)
                {
                    Err(ErrorCode::Changed) => {
                        // A second contradictory worker response is a session/protocol
                        // failure, not evidence that an unchanged source was edited.
                        if self.key(book, index, view, shared)? != key {
                            return Err(ErrorCode::Changed);
                        }
                        self.trace(
                            "reopened_worker_still_reports_changed_source_stable",
                            shared,
                        );
                        return Err(ErrorCode::Protocol);
                    }
                    result => {
                        let frame = result?;
                        self.trace("source_reopen_decode_succeeded", shared);
                        frame
                    }
                }
            }
            Err(ErrorCode::Limit) if !guard.prefetch && self.cache.bytes() > 0 => {
                self.trace("foreground_reclaim_cache", shared);
                self.cache.clear();
                self.publish_cache_stats(shared);
                self.worker(shared)?
                    .decode(&book.pages[index], view, shared, guard)?
            }
            other => other?,
        };
        if self.key(book, index, view, shared)? != key {
            self.trace("source_changed_during_decode", shared);
            return Err(ErrorCode::Changed);
        }
        if frame.info.animation.is_some() {
            if guard.prefetch {
                self.stop_worker();
            } else if let Some(worker) = self.worker.as_mut() {
                worker.animation_target = Some(key.clone());
                worker.animation_generation = guard.generation;
            }
        }
        {
            // Serialize insertion with new selections/open/close: no stale work resurrection.
            // Metadata I/O and decode are deliberately outside this short critical section.
            let pending = shared.pending.lock().unwrap();
            if !guard.allowed_locked(shared, &pending) {
                return Err(ErrorCode::Cancelled);
            }
            if frame.info.animation.is_none()
                && self
                    .cache
                    .insert(key, frame.clone(), guard.prefetch, &shared.budget)
                && guard.prefetch
            {
                shared.stats.prefetches.fetch_add(1, Ordering::Relaxed);
            }
            self.publish_cache_stats(shared);
        }
        Ok((frame, false))
    }

    fn plan(
        &mut self,
        book: Arc<Book>,
        index: usize,
        view: View,
        generation: u64,
    ) -> Option<PrefetchPlan> {
        if let Some((epoch, previous)) = self.previous
            && epoch == book.epoch
            && index != previous
        {
            self.forward = index > previous;
        }
        self.previous = Some((book.epoch, index));
        if self
            .worker
            .as_ref()
            .is_some_and(|w| w.animation_workspace.is_some())
            || !book.complete
            || !self.cache.enabled()
            || view.quarter_turns != 0
        {
            return None;
        }
        let adjacent = if self.forward {
            index.checked_add(1).filter(|i| *i < book.pages.len())
        } else {
            index.checked_sub(1)
        }?;
        Some(PrefetchPlan {
            book,
            index: adjacent,
            view,
            generation,
        })
    }

    fn run_job(
        &mut self,
        job: Job,
        shared: &Shared,
        sender: &async_channel::Sender<Event>,
    ) -> Result<Option<PrefetchPlan>, ErrorCode> {
        let generation = job.generation;
        self.trace("foreground_job", shared);
        match job.task {
            Task::Open { path, view } => {
                if self
                    .worker
                    .as_ref()
                    .is_some_and(|w| w.animation_workspace.is_some())
                {
                    self.stop_worker();
                }
                self.trace("open_source_requested", shared);
                self.cache.clear();
                self.publish_cache_stats(shared);
                self.previous = None;
                self.forward = true;
                let permit = Arc::new(shared.budget.reserve(INDEX_BYTES).ok_or(ErrorCode::Limit)?);
                let source = RawPath::new(&path);
                let mut opened =
                    self.worker(shared)?
                        .open(&source, shared, generation, permit.clone());
                // FUSE backends may finalize stat attributes on the first read.
                // No catalog/frame has been published yet: retry a fresh open
                // once, retaining all revision checks on the new snapshot.
                if matches!(opened, Err(ErrorCode::Changed)) && current(shared, generation) {
                    self.trace("open_revision_retry", shared);
                    self.stop_worker();
                    opened = self
                        .worker(shared)?
                        .open(&source, shared, generation, permit);
                }
                let mut book = opened?;
                emit(
                    sender,
                    shared,
                    Event::Catalog {
                        generation,
                        book: book.clone(),
                    },
                )?;
                if book.pages.is_empty() {
                    return Err(ErrorCode::Unsupported);
                }
                let guard = WorkGuard::new(
                    generation,
                    Some(Target::new(&book, book.selected, view)),
                    false,
                );
                match self.frame(&book, book.selected, view, shared, &guard) {
                    Ok((frame, cached)) => emit(
                        sender,
                        shared,
                        Event::Frame {
                            generation,
                            frame,
                            cached,
                        },
                    )?,
                    Err(code) if current(shared, generation) => {
                        self.trace(&format!("open_frame_error_{code:?}"), shared);
                        emit(sender, shared, Event::Error { generation, code })?
                    }
                    Err(code) => return Err(code),
                }
                if !book.complete && current(shared, generation) {
                    book = self
                        .worker(shared)?
                        .full_catalog(&book, shared, generation)?;
                    emit(
                        sender,
                        shared,
                        Event::Catalog {
                            generation,
                            book: book.clone(),
                        },
                    )?;
                }
                Ok(self.plan(book.clone(), book.selected, view, generation))
            }
            Task::Load { book, index, view } => {
                self.trace("load_page_requested", shared);
                let guard =
                    WorkGuard::new(generation, Some(Target::new(&book, index, view)), false);
                let (frame, cached) = self.frame(&book, index, view, shared, &guard)?;
                emit(
                    sender,
                    shared,
                    Event::Frame {
                        generation,
                        frame,
                        cached,
                    },
                )?;
                Ok(self.plan(book, index, view, generation))
            }
            Task::Animate {
                book,
                index,
                view,
                seek,
            } => {
                let key = self.key(&book, index, view, shared)?;
                let worker = self.worker.as_mut().ok_or(ErrorCode::Protocol)?;
                if worker.animation_target.as_ref() != Some(&key) {
                    return Err(ErrorCode::Cancelled);
                }
                let guard = WorkGuard::new(generation, None, false);
                let next =
                    worker.decode_frame(&book.pages[index], view, shared, &guard, Some(seek))?;
                worker.animation_generation = generation;
                if self.key(&book, index, view, shared)? != key {
                    return Err(ErrorCode::Changed);
                }
                match next {
                    Some(frame) => emit(
                        sender,
                        shared,
                        Event::Frame {
                            generation,
                            frame,
                            cached: false,
                        },
                    )?,
                    None => {
                        emit(sender, shared, Event::AnimationEnded { generation })?;
                    }
                }
                Ok(None)
            }
        }
    }

    fn prefetch(&mut self, plan: PrefetchPlan, shared: &Shared) {
        if !current(shared, plan.generation)
            || !shared.prefetch_enabled.load(Ordering::Acquire)
            || !self.cache.enabled()
            || shared.budget.used()
                >= shared
                    .budget
                    .limit()
                    .saturating_mul(options::PREFETCH_STOP_PERCENT)
                    / 100
        {
            return;
        }
        let Ok(key) = self.key(&plan.book, plan.index, plan.view, shared) else {
            return;
        };
        if self.cache.contains(&key) {
            return;
        }
        let guard = WorkGuard::new(plan.generation, Some(key.target), true);
        // Background failures never replace the current image or report a foreground error.
        if let Err(error) = self.frame(&plan.book, plan.index, plan.view, shared, &guard) {
            if matches!(
                error,
                ErrorCode::Changed | ErrorCode::Worker | ErrorCode::Protocol | ErrorCode::Timeout
            ) {
                self.trace(&format!("prefetch_error_{error:?}"), shared);
            } else {
                self.trace(&format!("prefetch_result_{error:?}"), shared);
            }
            if matches!(
                error,
                ErrorCode::Worker | ErrorCode::Protocol | ErrorCode::Timeout
            ) || self.worker.as_ref().is_some_and(|w| w.child.is_none())
            {
                self.stop_worker();
            }
        }
        self.publish_cache_stats(shared);
    }
}

fn coordinator(
    path: WorkerCommand,
    shared: Arc<Shared>,
    sender: async_channel::Sender<Event>,
    cache_bytes: u64,
) {
    let mut engine = Engine {
        path,
        worker: None,
        cache: FrameCache::new(cache_bytes),
        previous: None,
        forward: true,
    };
    let mut prefetch = None;
    enum Work {
        Foreground(Job),
        Neighbor(Neighbor),
        Prefetch(PrefetchPlan),
        StopAnimation,
    }
    loop {
        let work = {
            let mut pending = shared.pending.lock().unwrap();
            loop {
                if shared.closing.load(Ordering::Acquire) {
                    break None;
                }
                if let Some(job) = pending.take() {
                    break Some(Work::Foreground(job));
                }
                if let Some(neighbor) = shared.neighbor.lock().unwrap().take() {
                    break Some(Work::Neighbor(neighbor));
                }
                if engine.worker.as_ref().is_some_and(|w| {
                    w.animation_workspace.is_some() && !current(&shared, w.animation_generation)
                }) {
                    break Some(Work::StopAnimation);
                }
                if shared.prefetch_enabled.load(Ordering::Acquire)
                    && let Some(plan) = prefetch.take()
                {
                    break Some(Work::Prefetch(plan));
                }
                pending = shared.wake.wait(pending).unwrap();
            }
        };
        match work {
            None => break,
            Some(Work::StopAnimation) => engine.stop_worker(),
            Some(Work::Prefetch(plan)) => engine.prefetch(plan, &shared),
            Some(Work::Neighbor(Neighbor {
                request,
                book,
                forward,
            })) => {
                let current_request = || {
                    !shared.closing.load(Ordering::Acquire)
                        && shared.neighbor_request.load(Ordering::Acquire) == request
                };
                let source = book.source.path();
                let zip = book.pages.first().map_or_else(
                    || neighbors::archive_name(&source),
                    |page| matches!(page.key, PageKey::Zip(_)),
                );
                let result = neighbors::find(&source, zip, forward, current_request);
                if current_request() {
                    let _ = sender.send_blocking(Event::Neighbor {
                        generation: shared.generation.load(Ordering::Acquire),
                        request,
                        result,
                    });
                }
            }
            Some(Work::Foreground(job)) => {
                prefetch = None;
                let generation = job.generation;
                match engine.run_job(job, &shared, &sender) {
                    Ok(plan) => prefetch = plan,
                    Err(code) => {
                        if code == ErrorCode::Cancelled {
                            engine.trace("foreground_cancelled", &shared);
                        } else {
                            engine.trace(&format!("foreground_error_{code:?}"), &shared);
                        }
                        if code != ErrorCode::Cancelled
                            || engine.worker.as_ref().is_some_and(|w| w.child.is_none())
                        {
                            engine.stop_worker();
                        }
                        if current(&shared, generation) {
                            let _ = emit(&sender, &shared, Event::Error { generation, code });
                        }
                    }
                }
            }
        }
    }
    drop(prefetch);
    engine.stop_worker();
    engine.cache.clear();
    engine.publish_cache_stats(&shared);
    drop(engine);
    shared.finished.store(true, Ordering::Release);
}

/// The worker must be an explicitly configured binary or the sibling of the GUI.
/// Never search PATH or the opened book's directory for an executable.
pub fn worker_path() -> io::Result<PathBuf> {
    if let Some(path) = std::env::var_os("NCAT_WORKER") {
        return Ok(PathBuf::from(path));
    }
    Ok(std::env::current_exe()?
        .parent()
        .ok_or_else(|| io::Error::other("executable directory"))?
        .join(ncat_platform::worker_filename()))
}

#[cfg(test)]
mod guard_tests {
    use super::*;

    fn test_shared() -> Arc<Shared> {
        let options = BackendOptions::default();
        Arc::new(Shared {
            pending: Mutex::new(None),
            neighbor: Mutex::new(None),
            neighbor_request: AtomicU64::new(0),
            diagnostics: Mutex::new(diagnostics::Diagnostics::new()),
            wake: Condvar::new(),
            generation: AtomicU64::new(1),
            closing: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            budget: Budget::new(options.budget),
            stats: Stats::default(),
            prefetch_enabled: AtomicBool::new(true),
            options,
        })
    }

    #[test]
    fn drop_diagnostics_are_bounded_and_do_not_submit_or_cancel_work() {
        let shared = test_shared();
        let (_, events) = async_channel::bounded(1);
        let backend = Backend {
            shared: shared.clone(),
            events,
        };
        for generation in 0..200 {
            backend.diagnose_drop(DropEvent::OpenQueued { generation });
        }
        let log = backend.diagnostic_log();
        assert_eq!(log.lines().count(), 128);
        assert!(log.lines().next().unwrap().ends_with("generation: 72 }"));
        assert!(log.ends_with("generation: 199 }"));
        assert!(shared.pending.lock().unwrap().is_none());
        assert_eq!(backend.generation(), 1);
        backend.shutdown();
        backend.diagnose_drop(DropEvent::Left);
        assert_eq!(backend.diagnostic_log(), log);
    }

    #[test]
    fn only_identical_live_target_can_promote_background_work() {
        let shared = test_shared();
        let book = Arc::new(Book {
            source: RawPath(vec![b'/']),
            title: String::new(),
            pages: vec![Page {
                key: ncat_protocol::PageKey::Zip(0),
                name: String::new(),
            }],
            selected: 0,
            complete: true,
            index_permit: Arc::new(shared.budget.reserve(1).unwrap()),
            epoch: 1,
            source_revision: None,
        });
        let view = View::default();
        let guard = WorkGuard::new(1, Some(Target::new(&book, 0, view)), true);
        assert!(guard.allowed(&shared));
        shared.generation.store(2, Ordering::Release);
        assert!(!guard.allowed(&shared));
        *shared.pending.lock().unwrap() = Some(Job {
            generation: 2,
            task: Task::Load {
                book: book.clone(),
                index: 0,
                view,
            },
        });
        assert!(guard.allowed(&shared));
        assert!(guard.allowed(&shared));
        assert_eq!(shared.stats.snapshot().promotions, 1);
        *shared.pending.lock().unwrap() = Some(Job {
            generation: 2,
            task: Task::Load {
                book,
                index: 0,
                view: View { width: 40, ..view },
            },
        });
        assert!(!guard.allowed(&shared));
        shared.prefetch_enabled.store(false, Ordering::Release);
        shared.generation.store(1, Ordering::Release);
        assert!(!guard.allowed(&shared));
        shared.prefetch_enabled.store(true, Ordering::Release);
        shared.closing.store(true, Ordering::Release);
        assert!(!guard.allowed(&shared));
    }

    #[test]
    fn cancel_invalidates_pending_work_without_shutting_down() {
        let shared = test_shared();
        let (_, events) = async_channel::bounded(1);
        let backend = Backend {
            shared: shared.clone(),
            events,
        };
        let generation = backend
            .open(PathBuf::from("/not-read/book.cbz"), View::default())
            .unwrap();
        assert!(shared.pending.lock().unwrap().is_some());
        backend.cancel();
        assert!(shared.pending.lock().unwrap().is_none());
        assert!(backend.generation() > generation);
        assert!(!shared.closing.load(Ordering::Acquire));
        assert!(
            backend
                .open(PathBuf::from("/not-read/next.cbz"), View::default())
                .is_ok()
        );
    }
}
