// SPDX-License-Identifier: MIT

use std::{fs, time::Duration};

mod archive;
mod cache;
mod cancellation;
mod cover_preview;
mod heavy_queue;
mod model_preview;
mod pdf;
mod remote_preview;
mod resolution;
mod route_preview;

use super::*;
use crate::services::{MediaPreviewSize, PreviewContent};

type RecordedEvents = Rc<RefCell<Vec<PreviewEvent>>>;

fn preview_entry(location: crate::model::Location, name: &str) -> crate::model::FileEntry {
    use crate::model::{EntryKind, FileEntry, MetadataValue};

    FileEntry {
        location,
        thumbnail_path: None,
        native_name: name.into(),
        display_name: name.into(),
        kind: EntryKind::File,
        size: MetadataValue::Unknown,
        modified_unix_seconds: MetadataValue::Unknown,
        created_unix_seconds: MetadataValue::Unknown,
        mode: MetadataValue::Unknown,
        recent_unix_seconds: MetadataValue::Unknown,
        is_hidden: false,
        image_dimensions: MetadataValue::Unknown,
        child_count: MetadataValue::Unknown,
        duration_seconds: MetadataValue::Unknown,
        listable: MetadataValue::Unknown,
        recent_uri: None,
    }
}

fn preview_request(location: crate::model::Location, name: &str) -> PreviewRequest {
    PreviewRequest {
        id: crate::services::PreviewRequestId(1),
        entry: preview_entry(location, name),
        text_byte_limit: 1024,
        render_document: false,
        pdf_page: 0,
        media_size: MediaPreviewSize::new(640, 800),
        detail: Default::default(),
        model_palette: crate::services::ModelPalette::default(),
        model_view: Default::default(),
        archive_password: None,
    }
}

fn software_provider() -> LocalPreviewProvider {
    LocalPreviewProvider::new(Rc::new(|| MediaPreviewBackend::Software))
}

fn parse_output(data: Vec<u8>, page: i32, pages: i32) -> crate::sandbox::ParseOutput {
    crate::sandbox::ParseOutput {
        data,
        page,
        pages,
        text_layer: None,
        model_mesh: None,
    }
}

fn event_recorder() -> (RecordedEvents, Rc<dyn Fn(PreviewEvent)>) {
    let events = RecordedEvents::default();
    let events_for_emit = events.clone();
    (
        events,
        Rc::new(move |event| events_for_emit.borrow_mut().push(event)),
    )
}

fn wait_for_first_event(context: &glib::MainContext, events: &RecordedEvents) {
    context.block_on(async {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while events.borrow().is_empty() && std::time::Instant::now() < deadline {
            glib::timeout_future(Duration::from_millis(1)).await;
        }
    });
}

/// Loads `request` with an injected renderer and returns the events seen
/// until the first one arrives or five seconds pass.
fn load_until_first_event(
    provider: &LocalPreviewProvider,
    context: &glib::MainContext,
    request: PreviewRequest,
    render: impl FnOnce(
        &Path,
        ParseOperation,
        i32,
        MediaPreviewBackend,
        &Cancellation,
    ) -> Result<crate::sandbox::ParseOutput, String>
    + Send
    + 'static,
) -> Vec<PreviewEvent> {
    let (events, emit) = event_recorder();
    let _handle = provider.load_with_renderer(request, emit, render);
    wait_for_first_event(context, &events);
    events.borrow().clone()
}

/// Runs `test` while owning the shared default main context.
fn with_default_main_context(test: impl FnOnce(&LocalPreviewProvider, &glib::MainContext)) {
    let _main_context = crate::test_support::ASYNC_MAIN_CONTEXT_DEFAULT
        .lock()
        .expect("main context lock");
    let context = glib::MainContext::default();
    let _owner = context.acquire().expect("main context owner");
    test(&software_provider(), &context);
}

fn heavy_preview_is_waiting(waiter: &mut HeavyPreviewWaiter) -> bool {
    waiter
        .receive
        .as_mut()
        .expect("heavy preview receiver")
        .try_recv()
        .expect("heavy preview receiver open")
        .is_none()
}

mod neighbor_preview {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    };

    use super::*;
    use crate::{
        model::{EntryKind, FileEntry, Location, MetadataValue},
        services::PreviewRequestId,
    };

    fn entry(path: &Path, name: &str) -> FileEntry {
        FileEntry {
            location: Location::local(path),
            thumbnail_path: None,
            native_name: name.into(),
            display_name: name.into(),
            kind: EntryKind::File,
            size: MetadataValue::Unknown,
            modified_unix_seconds: MetadataValue::Known(1),
            created_unix_seconds: MetadataValue::Unknown,
            mode: MetadataValue::Unknown,
            recent_unix_seconds: MetadataValue::Unknown,
            recent_uri: None,
            is_hidden: false,
            image_dimensions: MetadataValue::Unknown,
            child_count: MetadataValue::Unknown,
            duration_seconds: MetadataValue::Unknown,
            listable: MetadataValue::Unknown,
        }
    }

    fn request(entry: &FileEntry, id: u64) -> PreviewRequest {
        PreviewRequest {
            id: PreviewRequestId(id),
            entry: entry.clone(),
            text_byte_limit: 1024,
            render_document: false,
            pdf_page: 0,
            media_size: MediaPreviewSize::new(640, 800),
            detail: Default::default(),
            model_palette: crate::services::ModelPalette::default(),
            model_view: Default::default(),
            archive_password: None,
        }
    }

    fn output(data: &[u8]) -> crate::sandbox::ParseOutput {
        crate::sandbox::ParseOutput {
            data: data.to_vec(),
            page: 0,
            pages: 3,
            text_layer: None,
            model_mesh: None,
        }
    }

    fn provider() -> LocalPreviewProvider {
        LocalPreviewProvider::new(Rc::new(|| MediaPreviewBackend::Software))
    }

    fn pump(condition: impl Fn() -> bool) {
        let context = glib::MainContext::default();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(std::time::Instant::now() < deadline, "preview deadline");
            context.iteration(false);
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn cached(path: &Path, pdf: bool) -> bool {
        PREVIEW_CACHE.with(|cache| {
            cache
                .borrow()
                .entries
                .keys()
                .any(|key| key.path == path && key.pdf_page.is_some() == pdf)
        })
    }

    type EventLog = Rc<RefCell<Vec<PreviewEvent>>>;

    fn events() -> (EventLog, Rc<dyn Fn(PreviewEvent)>) {
        let events = Rc::new(RefCell::new(Vec::new()));
        let sink = events.clone();
        (events, Rc::new(move |event| sink.borrow_mut().push(event)))
    }

    #[test]
    fn heavy_lanes_serve_interactive_then_neighbor_then_later_pages() {
        let context = crate::test_support::PrivateContext::new();
        context.block_on(async {
            let running = request_heavy_preview_permit()
                .acquire()
                .await
                .expect("running render");
            let later = request_ranked_heavy_permit(HeavyRank::LaterPage);
            let neighbor = request_ranked_heavy_permit(HeavyRank::Neighbor);
            let current = request_ranked_heavy_permit(HeavyRank::Interactive);
            let mut order = Vec::new();
            drop(running);
            let held = current.acquire().await.expect("interactive first");
            order.push("current");
            drop(held);
            let held = neighbor
                .acquire()
                .await
                .expect("neighbor before later pages");
            order.push("neighbor");
            drop(held);
            drop(later.acquire().await.expect("later page last"));
            order.push("later");
            assert_eq!(order, ["current", "neighbor", "later"]);
        });
        HEAVY_PREVIEW_QUEUE.with(|queue| {
            let queue = queue.borrow();
            assert_eq!(queue.running, 0);
            assert!(queue.queued.is_empty());
        });
    }

    #[test]
    fn interactive_work_preempts_a_running_neighbor_but_later_pages_do_not() {
        let context = crate::test_support::PrivateContext::new();
        context.block_on(async {
            let key = PreviewCacheKey {
                path: "/neighbor.pdf".into(),
                modified: 1,
                pdf_page: None,
                model: None,
                route: None,
                expanded: false,
            };
            let cancel = Cancellation::default();
            let neighbor = request_ranked_heavy_permit(HeavyRank::Neighbor)
                .acquire()
                .await
                .expect("neighbor permit");
            mark_neighbor_running(&key, &cancel);
            let later = request_ranked_heavy_permit(HeavyRank::LaterPage);
            assert!(
                !cancel.is_cancelled(),
                "a later page waits for the neighbor"
            );
            let current = request_ranked_heavy_permit(HeavyRank::Interactive);
            assert!(cancel.is_cancelled(), "interactive work never waits for it");
            drop((neighbor, later, current));
        });
    }

    #[test]
    fn a_started_neighbor_render_fills_the_cache_after_its_handle_is_dropped() {
        crate::test_support::gtk_test(
            "adapters::local_preview::tests::neighbor_preview::a_started_neighbor_render_fills_the_cache_after_its_handle_is_dropped",
            || {
                let directory = tempfile::tempdir().expect("fixture directory");
                let path = directory.path().join("next.png");
                let entry = entry(&path, "next.png");
                let context = glib::MainContext::default();
                let _owner = context.acquire().expect("main context owner");
                let (started, release) = (Arc::new(AtomicBool::new(false)), mpsc::channel::<()>());
                let signal = started.clone();
                let (neighbor_events, emit) = events();
                let handle = provider().load_prioritized(
                    request(&entry, 1),
                    emit,
                    move |_, _, _, _, _| {
                        signal.store(true, Ordering::SeqCst);
                        release
                            .1
                            .recv_timeout(Duration::from_secs(5))
                            .expect("release");
                        Ok(output(b"neighbor png"))
                    },
                    PreviewPriority::Neighbor,
                );
                pump(|| started.load(Ordering::SeqCst));
                drop(handle);
                release.0.send(()).expect("release the render");
                pump(|| cached(&path, false));
                assert!(
                    neighbor_events.borrow().len() <= 1,
                    "the dropped neighbor may still report, harmlessly"
                );

                let (current_events, emit) = events();
                let _current =
                    provider().load_with_renderer(request(&entry, 2), emit, |_, _, _, _, _| {
                        panic!("the interactive request must hit the cache")
                    });
                pump(|| !current_events.borrow().is_empty());
                let events = current_events.borrow();
                let PreviewEvent::Ready(preview) = &events[0] else {
                    panic!("current preview failed");
                };
                assert_eq!(
                    preview.content,
                    PreviewContent::Rasterized {
                        png: b"neighbor png".to_vec()
                    }
                );
            },
        );
    }

    #[test]
    fn an_interactive_request_joins_a_running_neighbor_pdf_render() {
        crate::test_support::gtk_test(
            "adapters::local_preview::tests::neighbor_preview::an_interactive_request_joins_a_running_neighbor_pdf_render",
            || {
                let directory = tempfile::tempdir().expect("fixture directory");
                let path = directory.path().join("next.pdf");
                let entry = entry(&path, "next.pdf");
                let context = glib::MainContext::default();
                let _owner = context.acquire().expect("main context owner");
                let renders = Arc::new(AtomicUsize::new(0));
                let (started, release) = (Arc::new(AtomicBool::new(false)), mpsc::channel::<()>());
                let (counter, signal) = (renders.clone(), started.clone());
                let (_neighbor_events, emit) = events();
                let _neighbor = provider().load_prioritized(
                    request(&entry, 1),
                    emit,
                    move |_, _, _, _, _| {
                        counter.fetch_add(1, Ordering::SeqCst);
                        signal.store(true, Ordering::SeqCst);
                        release
                            .1
                            .recv_timeout(Duration::from_secs(5))
                            .expect("release");
                        Ok(output(b"page"))
                    },
                    PreviewPriority::Neighbor,
                );
                pump(|| started.load(Ordering::SeqCst));

                let counter = renders.clone();
                let (current_events, emit) = events();
                let _current = provider().load_with_renderer(
                    request(&entry, 2),
                    emit,
                    move |_, _, _, _, _| {
                        counter.fetch_add(1, Ordering::SeqCst);
                        Ok(output(b"duplicate"))
                    },
                );
                for _ in 0..20 {
                    context.iteration(false);
                    std::thread::sleep(Duration::from_millis(2));
                }
                assert!(current_events.borrow().is_empty(), "waits for the neighbor");
                release.0.send(()).expect("release the render");
                pump(|| !current_events.borrow().is_empty());
                let events = current_events.borrow();
                let PreviewEvent::Ready(preview) = &events[0] else {
                    panic!("current PDF failed");
                };
                assert!(
                    matches!(&preview.content, PreviewContent::Pdf { png, .. } if png == b"page")
                );
                assert_eq!(renders.load(Ordering::SeqCst), 1, "rendered once");
            },
        );
    }

    #[test]
    fn a_queued_neighbor_gives_way_to_the_interactive_request_for_its_page() {
        crate::test_support::gtk_test(
            "adapters::local_preview::tests::neighbor_preview::a_queued_neighbor_gives_way_to_the_interactive_request_for_its_page",
            || {
                let directory = tempfile::tempdir().expect("fixture directory");
                let path = directory.path().join("next.pdf");
                let entry = entry(&path, "next.pdf");
                let context = glib::MainContext::default();
                let _owner = context.acquire().expect("main context owner");
                let busy = context.block_on(request_heavy_preview_permit().acquire());
                let renders = Arc::new(AtomicUsize::new(0));
                let counter = renders.clone();
                let (_neighbor_events, emit) = events();
                let _neighbor = provider().load_prioritized(
                    request(&entry, 1),
                    emit,
                    move |_, _, _, _, _| {
                        counter.fetch_add(10, Ordering::SeqCst);
                        Ok(output(b"neighbor"))
                    },
                    PreviewPriority::Neighbor,
                );
                pump(|| NEIGHBOR_RENDERS.with(|renders| !renders.borrow().is_empty()));

                let counter = renders.clone();
                let (current_events, emit) = events();
                let _current = provider().load_with_renderer(
                    request(&entry, 2),
                    emit,
                    move |_, _, _, _, _| {
                        counter.fetch_add(1, Ordering::SeqCst);
                        Ok(output(b"current"))
                    },
                );
                for _ in 0..20 {
                    context.iteration(false);
                    std::thread::sleep(Duration::from_millis(2));
                }
                drop(busy);
                pump(|| !current_events.borrow().is_empty());
                pump(|| NEIGHBOR_RENDERS.with(|renders| renders.borrow().is_empty()));
                assert_eq!(
                    renders.load(Ordering::SeqCst),
                    1,
                    "only the interactive render runs"
                );
            },
        );
    }

    #[test]
    fn neighbors_only_prepare_local_images_pdfs_and_media() {
        crate::test_support::gtk_test(
            "adapters::local_preview::tests::neighbor_preview::neighbors_only_prepare_local_images_pdfs_and_media",
            || {
                let directory = tempfile::tempdir().expect("fixture directory");
                let context = glib::MainContext::default();
                let _owner = context.acquire().expect("main context owner");
                let renders = Arc::new(AtomicUsize::new(0));
                let (all_events, emit) = events();
                let mut handles = Vec::new();
                for name in ["notes.txt", "data.json", "model.stl", "archive.zip"] {
                    let counter = renders.clone();
                    let path = directory.path().join(name);
                    handles.push(provider().load_prioritized(
                        request(&entry(&path, name), 1),
                        emit.clone(),
                        move |_, _, _, _, _| {
                            counter.fetch_add(1, Ordering::SeqCst);
                            Ok(output(b"unexpected"))
                        },
                        PreviewPriority::Neighbor,
                    ));
                }
                for _ in 0..30 {
                    context.iteration(false);
                    std::thread::sleep(Duration::from_millis(2));
                }
                assert_eq!(renders.load(Ordering::SeqCst), 0);
                assert!(all_events.borrow().is_empty());
            },
        );
    }
}
