//! Pipeline tests drive fake workers through POSIX shell scripts and raw non-UTF-8
//! paths, so they run on Unix only.
#![cfg(unix)]

use image::{ImageBuffer, Rgba};
use ncat_backend::{Backend, BackendOptions, Book, Event, Frame, View};
use ncat_core::{DEFAULT_BUDGET, MIB};
use ncat_protocol::ErrorCode;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::unix::{ffi::OsStringExt, fs::PermissionsExt};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn backend() -> Backend {
    Backend::start(env!("CARGO_BIN_EXE_ncat-worker").into()).unwrap()
}

fn png(path: &Path, width: u32, height: u32) {
    ImageBuffer::<Rgba<u8>, Vec<u8>>::from_pixel(width, height, Rgba([240, 20, 40, 255]))
        .save_with_format(path, image::ImageFormat::Png)
        .unwrap();
}

fn event(backend: &Backend) -> Event {
    let rx = backend.events();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match rx.try_recv() {
            Ok(event) if event.generation() == backend.generation() => return event,
            Ok(_) => {}
            Err(async_error) if async_error.is_closed() => panic!("events closed"),
            Err(_) => {}
        }
        assert!(Instant::now() < deadline, "event deadline");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn frame(backend: &Backend) -> Arc<Frame> {
    loop {
        match event(backend) {
            Event::Frame { frame, .. } => return frame,
            Event::Error { code, .. } => panic!("decode: {code:?}"),
            _ => {}
        }
    }
}

fn closed(backend: &Backend) {
    backend.shutdown();
    let deadline = Instant::now() + Duration::from_millis(1500);
    while !backend.is_closed() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(backend.is_closed(), "shutdown exceeded grace");
}

fn catalog(backend: &Backend) -> Arc<Book> {
    loop {
        match event(backend) {
            Event::Catalog { book, .. } if book.complete => return book,
            Event::Error { code, .. } => panic!("catalog: {code:?}"),
            _ => {}
        }
    }
}

fn configured(cache_bytes: u64, prefetch: bool) -> Backend {
    Backend::with_options(
        env!("CARGO_BIN_EXE_ncat-worker").into(),
        BackendOptions {
            cache_bytes,
            prefetch,
            ..BackendOptions::default()
        },
    )
    .unwrap()
}

#[test]
fn configured_deadline_kills_hanging_worker_and_releases_reservations() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("hang");
    fs::write(&script, b"#!/bin/sh\nexec sleep 30\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let backend = Backend::with_options(
        script,
        BackendOptions {
            task_timeout: Duration::from_secs(1),
            ..BackendOptions::default()
        },
    )
    .unwrap();
    let start = Instant::now();
    backend.open(dir.path(), View::default()).unwrap();
    assert!(matches!(
        event(&backend),
        Event::Error {
            code: ErrorCode::Timeout,
            ..
        }
    ));
    assert!(start.elapsed() < Duration::from_secs(3));
    closed(&backend);
    assert_eq!(backend.budget_used(), 0);
}

// Fault-inject only the worker response, not filesystem timestamps. The source stays
// unchanged so the parent must distinguish a stale session from a real file edit.
fn changed_response_worker(root: &Path, persistent: bool) -> std::path::PathBuf {
    use ncat_protocol::{Envelope, Page, PageKey, Response, write_message};
    let replies = root.join("responses");
    let mut out = File::create(&replies).unwrap();
    write_message(
        &mut out,
        &Envelope::new(
            1,
            Response::Opened {
                title: String::new(),
                deferred: false,
            },
        ),
    )
    .unwrap();
    write_message(
        &mut out,
        &Envelope::new(
            1,
            Response::Pages {
                pages: vec![Page {
                    key: PageKey::Zip(0),
                    name: "1.png".into(),
                }],
                selected: 0,
                done: true,
            },
        ),
    )
    .unwrap();
    write_message(
        &mut out,
        &Envelope::new(2, Response::Error(ErrorCode::Changed)),
    )
    .unwrap();
    let quote = |path: &Path| format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"));
    let marker = root.join("started");
    let delegate = if persistent {
        String::new()
    } else {
        format!(
            "if test -f {}; then exec {}; fi\ntouch {}\n",
            quote(&marker),
            quote(Path::new(env!("CARGO_BIN_EXE_ncat-worker"))),
            quote(&marker)
        )
    };
    let script = root.join("changed-worker");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\n{delegate}cat {}\nexec sleep 30\n",
            quote(&replies)
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    script
}

#[test]
fn unchanged_source_recovers_from_stale_worker_revision_once() {
    let dir = tempfile::tempdir().unwrap();
    let archive = two_page_zip(dir.path());
    let script = changed_response_worker(dir.path(), false);
    let backend = Backend::with_options(
        script,
        BackendOptions {
            prefetch: false,
            ..BackendOptions::default()
        },
    )
    .unwrap();
    backend.open(&archive, View::default()).unwrap();
    let book = catalog(&backend);
    let image = frame(&backend);
    assert_eq!(image.info.original_width, 30);
    assert_eq!(backend.cache_stats().source_reopens, 1);
    assert_eq!(backend.cache_stats().worker_starts, 2);
    drop((image, book));
    closed(&backend);
    assert_eq!(backend.budget_used(), 0);
}

#[test]
fn repeated_worker_revision_failure_has_a_bounded_retry() {
    let dir = tempfile::tempdir().unwrap();
    let archive = two_page_zip(dir.path());
    let script = changed_response_worker(dir.path(), true);
    let backend = Backend::with_options(
        script,
        BackendOptions {
            prefetch: false,
            ..BackendOptions::default()
        },
    )
    .unwrap();
    backend.open(&archive, View::default()).unwrap();
    drop(catalog(&backend));
    assert!(matches!(
        event(&backend),
        Event::Error {
            code: ErrorCode::Protocol,
            ..
        }
    ));
    assert_eq!(backend.cache_stats().source_reopens, 1);
    assert_eq!(backend.cache_stats().worker_starts, 2);
    closed(&backend);
    assert_eq!(backend.budget_used(), 0);
}

#[test]
fn rapid_zip_navigation_keeps_current_page_without_false_source_changes() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("rapid.zip");
    let mut writer = zip::ZipWriter::new(File::create(&archive).unwrap());
    for index in 0..64 {
        let path = dir.path().join("page.png");
        png(&path, 10 + index, 24);
        writer
            .start_file(
                format!("{index}.png"),
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        writer.write_all(&fs::read(path).unwrap()).unwrap();
    }
    writer.finish().unwrap();
    let before = fs::read(&archive).unwrap();
    let backend = backend();
    backend.open(&archive, View::default()).unwrap();
    let book = catalog(&backend);
    let mut displayed = frame(&backend);
    assert_eq!(displayed.info.original_width, 10);
    for round in 0..50 {
        let mut last = 0;
        for offset in 0..5 {
            last = (round * 5 + offset) * 13 % book.pages.len();
            backend.load(book.clone(), last, View::default()).unwrap();
            std::thread::sleep(Duration::from_millis(1));
        }
        displayed = frame(&backend);
        assert_eq!(displayed.info.original_width as usize, 10 + last);
    }
    assert_eq!(backend.cache_stats().source_reopens, 0);
    assert_eq!(fs::read(archive).unwrap(), before);
    drop((book, displayed));
    closed(&backend);
    assert_eq!(backend.budget_used(), 0);
}

#[test]
fn folder_cache_reuses_pixels_ignores_subfolders_and_invalidates_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("1.png");
    png(&path, 16, 24);
    fs::create_dir(dir.path().join("nested")).unwrap();
    png(&dir.path().join("nested/2.png"), 80, 90);
    let backend = configured(MIB, false);
    backend.open(dir.path(), View::default()).unwrap();
    let book = catalog(&backend);
    assert_eq!(book.pages.len(), 1);
    let first = frame(&backend);
    backend.load(book.clone(), 0, View::default()).unwrap();
    assert!(Arc::ptr_eq(&first, &frame(&backend)));
    assert_eq!(backend.cache_stats().decodes, 1);
    assert_eq!(backend.cache_stats().hits, 1);
    png(&path, 20, 28);
    backend.load(book.clone(), 0, View::default()).unwrap();
    let changed = frame(&backend);
    assert_eq!(changed.info.original_width, 20);
    assert!(!Arc::ptr_eq(&first, &changed));
    fs::remove_file(path).unwrap();
    backend.load(book.clone(), 0, View::default()).unwrap();
    assert!(matches!(
        event(&backend),
        Event::Error {
            code: ErrorCode::Io,
            ..
        }
    ));
    drop((book, first, changed));
    closed(&backend);
    assert_eq!(backend.budget_used(), 0);
}

fn two_page_zip(root: &Path) -> std::path::PathBuf {
    let image = root.join("source.png");
    png(&image, 30, 40);
    let pixels = fs::read(image).unwrap();
    let archive = root.join("book.cbz");
    let mut writer = zip::ZipWriter::new(File::create(&archive).unwrap());
    for name in ["1.png", "2.png"] {
        writer
            .start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(&pixels).unwrap();
    }
    writer.finish().unwrap();
    archive
}

#[test]
fn zip_cache_reuses_frames_and_reopen_view_rotation_are_isolated() {
    let dir = tempfile::tempdir().unwrap();
    let archive = two_page_zip(dir.path());
    let backend = configured(MIB, false);
    backend.open(&archive, View::default()).unwrap();
    let book = catalog(&backend);
    let first = frame(&backend);
    backend.load(book.clone(), 1, View::default()).unwrap();
    drop(frame(&backend));
    backend.load(book.clone(), 0, View::default()).unwrap();
    assert!(Arc::ptr_eq(&first, &frame(&backend)));
    for view in [
        View {
            width: 10,
            ..View::default()
        },
        View {
            quarter_turns: 1,
            ..View::default()
        },
    ] {
        backend.load(book.clone(), 0, view).unwrap();
        assert!(!Arc::ptr_eq(&first, &frame(&backend)));
    }
    backend.open(&archive, View::default()).unwrap();
    drop(catalog(&backend));
    assert!(!Arc::ptr_eq(&first, &frame(&backend)));
    drop((book, first));
    closed(&backend);
    assert_eq!(backend.budget_used(), 0);
}

#[test]
fn folder_and_zip_prefetch_adjacent_page_without_foreground_redecode() {
    for zip in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let source = if zip {
            two_page_zip(dir.path())
        } else {
            png(&dir.path().join("1.png"), 30, 40);
            png(&dir.path().join("2.png"), 30, 40);
            dir.path().to_owned()
        };
        let backend = configured(MIB, true);
        backend.open(source, View::default()).unwrap();
        let book = catalog(&backend);
        drop(frame(&backend));
        let deadline = Instant::now() + Duration::from_secs(5);
        while backend.cache_stats().prefetches == 0 {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(backend.cache_stats().decodes, 2);
        backend.load(book.clone(), 1, View::default()).unwrap();
        match event(&backend) {
            Event::Frame { cached: true, .. } => {}
            other => panic!("expected cache hit: {other:?}"),
        }
        assert_eq!(backend.cache_stats().prefetched_hits, 1);
        assert_eq!(backend.cache_stats().decodes, 2);
        backend.set_prefetch_enabled(false);
        drop(book);
        closed(&backend);
        assert_eq!(backend.budget_used(), 0);
    }
}

#[test]
fn tiny_cache_evicts_without_revoking_display_or_restarting_worker() {
    let dir = tempfile::tempdir().unwrap();
    png(&dir.path().join("1.png"), 30, 40);
    png(&dir.path().join("2.png"), 30, 40);
    let backend = configured(6000, false);
    backend.open(dir.path(), View::default()).unwrap();
    let book = catalog(&backend);
    let first = frame(&backend);
    backend.load(book.clone(), 1, View::default()).unwrap();
    drop(frame(&backend));
    assert!(backend.cache_stats().evictions >= 1);
    assert!(backend.cache_stats().bytes <= 6000);
    backend.load(book.clone(), 0, View::default()).unwrap();
    assert!(!Arc::ptr_eq(&first, &frame(&backend)));
    assert_eq!(first.as_ref().as_ref().len(), 4800);
    assert_eq!(backend.cache_stats().worker_starts, 1);
    drop((book, first));
    closed(&backend);
    assert_eq!(backend.budget_used(), 0);
}

#[test]
fn foreground_reclaims_optional_cache_before_reporting_memory_limit() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..5 {
        png(&dir.path().join(format!("{i}.png")), 512, 512);
    }
    let backend = Backend::with_options(
        env!("CARGO_BIN_EXE_ncat-worker").into(),
        BackendOptions {
            budget: 64 * MIB,
            cache_bytes: 8 * MIB,
            prefetch: false,
            ..BackendOptions::default()
        },
    )
    .unwrap();
    let view = View {
        width: 512,
        height: 512,
        quarter_turns: 0,
    };
    backend.open(dir.path(), view).unwrap();
    let book = catalog(&backend);
    drop(frame(&backend));
    for index in 1..5 {
        backend.load(book.clone(), index, view).unwrap();
        drop(frame(&backend));
        assert!(backend.budget_used() <= 64 * MIB);
    }
    assert!(backend.cache_stats().evictions > 0);
    assert_eq!(backend.cache_stats().worker_starts, 1);
    drop(book);
    closed(&backend);
    assert_eq!(backend.budget_used(), 0);
}

#[test]
fn selected_image_precedes_catalog_and_pixels_hold_permit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("p10.png");
    png(&path, 20, 30);
    png(&dir.path().join("p2.png"), 10, 20);
    let before = fs::read(&path).unwrap();
    let backend = backend();
    backend
        .open(
            &path,
            View {
                width: 10,
                height: 10,
                quarter_turns: 0,
            },
        )
        .unwrap();
    let first = match event(&backend) {
        Event::Catalog { book, .. } => book,
        _ => panic!("initial catalog"),
    };
    assert!(!first.complete);
    assert_eq!(first.pages.len(), 1);
    let image = frame(&backend);
    assert_eq!((image.info.width, image.info.height), (6, 10));
    assert_eq!(&image.as_ref().as_ref()[..4], &[240, 20, 40, 255]);
    let book = catalog(&backend);
    assert_eq!(
        book.pages
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        ["p2.png", "p10.png"]
    );
    assert_eq!(book.selected, 1);
    assert_eq!(fs::read(&path).unwrap(), before, "source was modified");
    drop(first);
    drop(book);
    closed(&backend);
    assert_eq!(backend.budget_used(), image.info.bytes * 3);
    drop(image);
    assert_eq!(backend.budget_used(), 0);
}

#[test]
fn zip_is_naturally_ordered_and_never_extracts_entry_paths() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("page.png");
    png(&path, 8, 12);
    let png = fs::read(&path).unwrap();
    let archive = dir.path().join("book.cbz");
    let mut writer = zip::ZipWriter::new(File::create(&archive).unwrap());
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for name in ["10.png", "2.png", "../outside.png", "__MACOSX/1.png"] {
        writer.start_file(name, options).unwrap();
        writer.write_all(&png).unwrap();
    }
    writer.finish().unwrap();
    let backend = backend();
    backend.open(&archive, View::default()).unwrap();
    let book = catalog(&backend);
    assert_eq!(
        book.pages
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        ["../outside.png", "2.png", "10.png"]
    );
    let first = frame(&backend);
    assert_eq!((first.info.width, first.info.height), (8, 12));
    backend.load(book.clone(), 2, View::default()).unwrap();
    let second = frame(&backend);
    assert_eq!(first.as_ref().as_ref(), second.as_ref().as_ref());
    assert!(!dir.path().join("outside.png").exists());
    assert!(!dir.path().join("10.png").exists());
    drop(book);
    drop(first);
    drop(second);
    closed(&backend);
    assert_eq!(backend.budget_used(), 0);
}

#[test]
fn rapid_navigation_only_publishes_current_generation() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..8 {
        png(&dir.path().join(format!("p{i}.png")), 10 + i, 20);
    }
    let backend = backend();
    backend.open(dir.path(), View::default()).unwrap();
    let book = catalog(&backend);
    drop(frame(&backend));
    for i in 0..100 {
        backend.load(book.clone(), i % 8, View::default()).unwrap();
    }
    let last = backend.load(book.clone(), 7, View::default()).unwrap();
    let image = frame(&backend);
    assert_eq!(backend.generation(), last);
    assert_eq!(image.info.original_width, 17);
    drop(image);
    drop(book);
    closed(&backend);
    assert_eq!(backend.budget_used(), 0);
}

#[test]
fn corrupt_page_does_not_block_next_page() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("1.png"), b"not an image").unwrap();
    png(&dir.path().join("2.png"), 5, 7);
    let backend = backend();
    backend.open(dir.path(), View::default()).unwrap();
    let book = catalog(&backend);
    assert!(matches!(event(&backend), Event::Error { .. }));
    backend.load(book.clone(), 1, View::default()).unwrap();
    let image = frame(&backend);
    assert_eq!((image.info.width, image.info.height), (5, 7));
    drop(book);
    drop(image);
    closed(&backend);
}

#[test]
fn non_utf8_filename_keeps_raw_identity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir
        .path()
        .join(std::ffi::OsString::from_vec(b"page\xff.png".to_vec()));
    png(&path, 2, 3);
    let backend = backend();
    backend.open(&path, View::default()).unwrap();
    let image = frame(&backend);
    assert_eq!(image.info.width, 2);
    drop(image);
    closed(&backend);
}

#[test]
fn insufficient_budget_is_an_error_not_an_unbounded_allocation() {
    let dir = tempfile::tempdir().unwrap();
    png(&dir.path().join("1.png"), 2, 3);
    let backend = Backend::with_budget(env!("CARGO_BIN_EXE_ncat-worker").into(), 16 * MIB).unwrap();
    backend.open(dir.path(), View::default()).unwrap();
    assert!(matches!(
        event(&backend),
        Event::Error {
            code: ErrorCode::Limit,
            ..
        }
    ));
    closed(&backend);
    assert_eq!(backend.budget_used(), 0);
}

#[test]
fn hanging_worker_is_killed_on_shutdown_without_ui_join() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("hang");
    fs::write(&script, b"#!/bin/sh\nexec sleep 60\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let backend = Backend::start(script).unwrap();
    backend.open(dir.path(), View::default()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while backend.budget_used() == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    let start = Instant::now();
    backend.shutdown();
    assert!(start.elapsed() < Duration::from_millis(100));
    closed(&backend);
    assert_eq!(backend.budget_used(), 0);
}

#[test]
fn worker_crash_is_reported_as_worker_failure_and_releases_budget() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("crash");
    fs::write(&script, b"#!/bin/sh\nexit 7\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let backend = Backend::start(script).unwrap();
    backend.open(dir.path(), View::default()).unwrap();
    assert!(matches!(
        event(&backend),
        Event::Error {
            code: ErrorCode::Worker,
            ..
        }
    ));
    closed(&backend);
    assert_eq!(backend.budget_used(), 0);
}

#[test]
fn rotated_output_is_bounded_and_old_leases_stay_valid() {
    let dir = tempfile::tempdir().unwrap();
    png(&dir.path().join("1.png"), 20, 30);
    let backend = backend();
    backend.open(dir.path(), View::default()).unwrap();
    let book = catalog(&backend);
    let first = frame(&backend);
    backend
        .load(
            book.clone(),
            0,
            View {
                quarter_turns: 1,
                ..View::default()
            },
        )
        .unwrap();
    let second = frame(&backend);
    assert_eq!((second.info.width, second.info.height), (30, 20));
    assert_eq!((first.info.width, first.info.height), (20, 30));
    assert!(backend.budget_used() <= DEFAULT_BUDGET);
    drop(book);
    drop(first);
    drop(second);
    closed(&backend);
    assert_eq!(backend.budget_used(), 0);
}

#[test]
fn malformed_ipc_is_rejected_without_echoing_input() {
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_ncat-worker"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&u32::MAX.to_le_bytes())
        .unwrap();
    let mut bytes = Vec::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(!child.wait().unwrap().success());
    assert!(bytes.is_empty());
}

#[test]
fn basic_static_codecs_use_the_same_worker_boundary() {
    for format in [
        image::ImageFormat::Jpeg,
        image::ImageFormat::WebP,
        image::ImageFormat::Bmp,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("signature-not-extension.dat");
        ImageBuffer::<image::Rgb<u8>, Vec<u8>>::from_pixel(9, 13, image::Rgb([120, 60, 30]))
            .save_with_format(&path, format)
            .unwrap();
        let before = fs::read(&path).unwrap();
        let backend = backend();
        backend.open(&path, View::default()).unwrap();
        let image = frame(&backend);
        assert_eq!((image.info.width, image.info.height), (9, 13));
        assert_eq!(fs::read(&path).unwrap(), before);
        drop(image);
        closed(&backend);
    }
}

#[test]
fn animated_gif_only_returns_first_composited_frame() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("animated.gif");
    {
        let mut encoder = image::codecs::gif::GifEncoder::new(File::create(&path).unwrap());
        for color in [Rgba([255, 0, 0, 255]), Rgba([0, 0, 255, 255])] {
            encoder
                .encode_frame(image::Frame::new(ImageBuffer::from_pixel(4, 6, color)))
                .unwrap();
        }
    }
    let backend = backend();
    backend.open(&path, View::default()).unwrap();
    let first = frame(&backend);
    assert_eq!(&first.as_ref().as_ref()[..4], &[255, 0, 0, 255]);
    assert_eq!(first.info.bytes, 4 * 6 * 4);
    drop(first);
    closed(&backend);
}

#[test]
fn large_output_is_capped_without_lying_about_original_dimensions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("large.png");
    png(&path, 2000, 3000);
    let backend = backend();
    backend
        .open(
            &path,
            View {
                width: 8192,
                height: 8192,
                quarter_turns: 0,
            },
        )
        .unwrap();
    let first = frame(&backend);
    assert_eq!(
        (first.info.original_width, first.info.original_height),
        (2000, 3000)
    );
    assert!(first.info.bytes <= ncat_protocol::MAX_OUTPUT);
    assert!(first.info.height < 3000);
    drop(first);
    closed(&backend);
}

#[test]
fn changed_zip_is_not_reused_as_the_original_source() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("1.png");
    png(&source, 4, 6);
    let archive = dir.path().join("book.zip");
    let mut writer = zip::ZipWriter::new(File::create(&archive).unwrap());
    writer
        .start_file("1.png", zip::write::SimpleFileOptions::default())
        .unwrap();
    writer.write_all(&fs::read(&source).unwrap()).unwrap();
    writer.finish().unwrap();
    let backend = backend();
    backend.open(&archive, View::default()).unwrap();
    let book = catalog(&backend);
    drop(frame(&backend));
    File::options()
        .append(true)
        .open(&archive)
        .unwrap()
        .write_all(b"changed")
        .unwrap();
    backend.load(book.clone(), 0, View::default()).unwrap();
    assert!(matches!(
        event(&backend),
        Event::Error {
            code: ErrorCode::Changed,
            ..
        }
    ));
    drop(book);
    closed(&backend);
}

#[test]
fn jpeg_exif_orientation_is_view_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rotated.jpg");
    ImageBuffer::<image::Rgb<u8>, Vec<u8>>::from_pixel(9, 13, image::Rgb([120, 60, 30]))
        .save_with_format(&path, image::ImageFormat::Jpeg)
        .unwrap();
    let original = fs::read(&path).unwrap();
    let exif = b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0";
    let mut with_exif = original[..2].to_vec();
    with_exif.extend_from_slice(&[0xff, 0xe1]);
    with_exif.extend_from_slice(&u16::try_from(exif.len() + 2).unwrap().to_be_bytes());
    with_exif.extend_from_slice(exif);
    with_exif.extend_from_slice(&original[2..]);
    fs::write(&path, &with_exif).unwrap();
    let backend = backend();
    backend.open(&path, View::default()).unwrap();
    let image = frame(&backend);
    assert_eq!(
        (image.info.original_width, image.info.original_height),
        (13, 9)
    );
    assert_eq!(fs::read(&path).unwrap(), with_exif);
    drop(image);
    closed(&backend);
}

#[test]
#[ignore = "manual 10,000-decode worker lifetime test; not the GUI/corpus release benchmark"]
fn ten_thousand_decodes_return_all_leases() {
    let dir = tempfile::tempdir().unwrap();
    png(&dir.path().join("1.png"), 4, 6);
    png(&dir.path().join("2.png"), 6, 4);
    let backend = Backend::with_options(
        env!("CARGO_BIN_EXE_ncat-worker").into(),
        BackendOptions {
            cache_bytes: 0,
            prefetch: false,
            ..BackendOptions::default()
        },
    )
    .unwrap();
    backend.open(dir.path(), View::default()).unwrap();
    let book = catalog(&backend);
    drop(frame(&backend));
    let baseline = backend.budget_used();
    for i in 0..10_000 {
        backend.load(book.clone(), i % 2, View::default()).unwrap();
        let image = frame(&backend);
        assert!(image.info.valid());
        drop(image);
        assert!(backend.budget_used() <= DEFAULT_BUDGET);
    }
    drop(book);
    closed(&backend);
    assert!(baseline > 0);
    assert_eq!(backend.budget_used(), 0);
}
