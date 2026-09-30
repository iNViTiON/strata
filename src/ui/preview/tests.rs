// SPDX-License-Identifier: MIT

use super::*;
use crate::model::{Location, MetadataValue};

struct Pending {
    request: PreviewRequest,
    emit: Rc<dyn Fn(PreviewEvent)>,
}

#[derive(Default)]
struct Provider(RefCell<Vec<Pending>>);

impl PreviewProvider for Provider {
    fn load(&self, request: PreviewRequest, emit: Rc<dyn Fn(PreviewEvent)>) -> LoadHandle {
        self.0.borrow_mut().push(Pending { request, emit });
        LoadHandle::new(|| {})
    }
}

fn entry(name: &str) -> FileEntry {
    FileEntry {
        location: Location::local(name),
        thumbnail_path: None,
        native_name: name.into(),
        display_name: name.into(),
        kind: EntryKind::File,
        size: MetadataValue::Unknown,
        modified_unix_seconds: MetadataValue::Unknown,
        mode: MetadataValue::Unknown,
        recent_unix_seconds: MetadataValue::Unknown,
        image_dimensions: MetadataValue::Unknown,
        child_count: MetadataValue::Unknown,
        duration_seconds: MetadataValue::Unknown,
        is_hidden: false,
    }
}

#[test]
fn comic_and_epub_selections_are_quick_preview_targets() {
    for name in ["sample.cbz", "sample.cbr", "sample.epub"] {
        assert!(preview_target(Some(entry(name))).is_some(), "{name}");
    }
}

#[test]
fn model_progress_and_theme_reloads_follow_the_current_request_in_each_drawer() {
    crate::test_support::gtk_test(
        "ui::preview::tests::model_progress_and_theme_reloads_follow_the_current_request_in_each_drawer",
        || {
            let provider = Rc::new(Provider::default());
            let first = PreviewDrawer::new(provider.clone(), false);
            let second = PreviewDrawer::new(provider.clone(), false);
            let manager = super::super::theme::ThemeManager::shared();
            first.show(entry("old.stl"), None);
            second.show(entry("second.stl"), None);
            first.show(entry("new.stl"), None);
            let emit_progress = |index: usize, stage| {
                let pending = provider.0.borrow();
                let pending = &pending[index];
                (pending.emit)(PreviewEvent::Progress {
                    request_id: pending.request.id,
                    stage,
                });
            };
            emit_progress(
                2,
                crate::services::ModelPreviewStage::Rendering { triangles: 23 },
            );
            let label = first
                .state
                .loading_label
                .borrow()
                .as_ref()
                .expect("loading feedback")
                .clone();
            assert_eq!(label.text(), "Rendering 23 triangles…");
            emit_progress(0, crate::services::ModelPreviewStage::Finishing);
            assert_eq!(label.text(), "Rendering 23 triangles…");
            {
                let pending = provider.0.borrow();
                (pending[0].emit)(PreviewEvent::Failed {
                    request_id: pending[0].request.id,
                    entry: entry("old.stl"),
                    message: "stale failure".into(),
                });
            }
            assert!(first.state.loading_label.borrow().is_some());
            let old_palette = manager.active_model_palette();
            let mut tokens = manager.appearance_tokens();
            tokens.accent = if old_palette.accent == 0xff0000 {
                "#00ff00"
            } else {
                "#ff0000"
            }
            .into();
            manager.preview(&tokens);
            {
                let pending = provider.0.borrow();
                assert_eq!(pending.len(), 5);
                assert_eq!(pending[3].request.entry.native_name, "new.stl");
                assert_eq!(pending[4].request.entry.native_name, "second.stl");
                for request in &pending[3..] {
                    assert_eq!(
                        request.request.model_palette,
                        manager.active_model_palette()
                    );
                    assert_ne!(request.request.model_palette, old_palette);
                }
            }
            first.close();
            emit_progress(3, crate::services::ModelPreviewStage::Finishing);
            assert!(first.state.current_request.get().is_none());
            assert!(first.state.loading_label.borrow().is_none());
            tokens.surface = "#102030".into();
            manager.preview(&tokens);
            assert_eq!(
                provider.0.borrow().len(),
                6,
                "closed drawer must not reload"
            );
            second.close();
        },
    );
}

#[test]
fn loads_ask_for_the_detail_the_preview_is_presented_at() {
    crate::test_support::gtk_test(
        "ui::preview::tests::loads_ask_for_the_detail_the_preview_is_presented_at",
        || {
            let provider = Rc::new(Provider::default());
            let drawer = PreviewDrawer::new(provider.clone(), false);
            drawer.show(entry("photo.png"), None);
            let pending = provider.0.borrow();
            assert_eq!(pending.len(), 1);
            assert_eq!(pending[0].request.detail, PreviewDetail::Standard);
        },
    );
}

#[test]
fn presentation_listeners_run_in_order_on_every_change() {
    crate::test_support::gtk_test(
        "ui::preview::tests::presentation_listeners_run_in_order_on_every_change",
        || {
            let drawer = PreviewDrawer::new(Rc::new(Provider::default()), false);
            let calls = Rc::new(RefCell::new(Vec::new()));
            for label in ["first", "second"] {
                let calls = calls.clone();
                drawer
                    .state
                    .on_presentation_changed(move || calls.borrow_mut().push(label));
            }
            drawer.state.presentation_changed();
            drawer.state.presentation_changed();
            assert_eq!(*calls.borrow(), ["first", "second", "first", "second"]);
        },
    );
}

mod preload {
    use crate::{
        sandbox::{MediaPreviewBackend, media::tests::stream_preload},
        services::{Preview, SandboxedMedia},
    };

    use super::*;

    const SIZE: MediaPreviewSize = MediaPreviewSize {
        width: 320,
        height: 240,
        expanded: false,
        max_fps: 30,
    };

    fn entry(name: &str) -> FileEntry {
        FileEntry {
            modified_unix_seconds: MetadataValue::Known(1),
            ..super::entry(name)
        }
    }

    #[derive(Default)]
    struct Recorder {
        preloads: RefCell<Vec<Pending>>,
        dropped: Rc<RefCell<Vec<String>>>,
    }

    impl PreviewProvider for Recorder {
        fn load(&self, _: PreviewRequest, _: Rc<dyn Fn(PreviewEvent)>) -> LoadHandle {
            LoadHandle::new(|| {})
        }

        fn preload(&self, request: PreviewRequest, emit: Rc<dyn Fn(PreviewEvent)>) -> LoadHandle {
            let name = request.entry.display_name.clone();
            self.preloads.borrow_mut().push(Pending { request, emit });
            let dropped = self.dropped.clone();
            LoadHandle::new(move || dropped.borrow_mut().push(name))
        }
    }

    fn drawer(enabled: bool) -> (PreviewDrawer, Rc<Recorder>) {
        let provider = Rc::new(Recorder::default());
        let drawer = PreviewDrawer::new(provider.clone(), false);
        drawer.state.preload.set_enabled(enabled);
        (drawer, provider)
    }

    fn drawer_with_saved_preference() -> (PreviewDrawer, Rc<Recorder>) {
        let provider = Rc::new(Recorder::default());
        (PreviewDrawer::new(provider.clone(), false), provider)
    }

    fn requested(provider: &Recorder) -> Vec<String> {
        provider
            .preloads
            .borrow()
            .iter()
            .map(|pending| pending.request.entry.display_name.clone())
            .collect()
    }

    fn ready(provider: &Recorder, name: &str, content: PreviewContent) {
        let pending = provider.preloads.borrow();
        let pending = pending
            .iter()
            .find(|pending| pending.request.entry.display_name == name)
            .expect("requested neighbor");
        (pending.emit)(PreviewEvent::Ready(Preview {
            request_id: pending.request.id,
            entry: pending.request.entry.clone(),
            content_type: String::new(),
            content,
        }));
    }

    fn png() -> PreviewContent {
        PreviewContent::Rasterized { png: vec![1] }
    }

    #[test]
    fn neighbors_are_requested_next_first_at_the_current_size() {
        crate::test_support::gtk_test(
            "ui::preview::tests::preload::neighbors_are_requested_next_first_at_the_current_size",
            || {
                let (drawer, provider) = drawer(true);
                let state = &drawer.state;
                state
                    .preload
                    .reconcile(state, [Some(entry("a.png")), Some(entry("b.pdf"))], SIZE);
                assert_eq!(requested(&provider), ["b.pdf", "a.png"]);
                let preloads = provider.preloads.borrow();
                assert!(preloads.iter().all(|pending| {
                    pending.request.pdf_page == 0 && pending.request.media_size == SIZE
                }));
                assert_ne!(preloads[0].request.id, preloads[1].request.id);
            },
        );
    }

    #[test]
    fn a_neighbor_waits_for_its_modified_time_and_is_then_prepared() {
        crate::test_support::gtk_test(
            "ui::preview::tests::preload::a_neighbor_waits_for_its_modified_time_and_is_then_prepared",
            || {
                let (drawer, provider) = drawer(true);
                let state = &drawer.state;
                let unread = FileEntry {
                    modified_unix_seconds: MetadataValue::Unknown,
                    ..entry("a.png")
                };
                assert!(
                    state.preload.reconcile(state, [Some(unread), None], SIZE),
                    "the caller is told to look again"
                );
                assert!(requested(&provider).is_empty());
                assert!(
                    !state
                        .preload
                        .reconcile(state, [Some(entry("a.png")), None], SIZE)
                );
                assert_eq!(requested(&provider), ["a.png"]);
            },
        );
    }

    #[test]
    fn only_local_images_pdfs_and_media_are_prepared() {
        crate::test_support::gtk_test(
            "ui::preview::tests::preload::only_local_images_pdfs_and_media_are_prepared",
            || {
                let (drawer, provider) = drawer(true);
                let state = &drawer.state;
                let mut folder = entry("folder");
                folder.kind = EntryKind::Directory;
                state
                    .preload
                    .reconcile(state, [Some(entry("notes.txt")), Some(folder)], SIZE);
                state.preload.reconcile(
                    state,
                    [Some(entry("part.stl")), Some(entry("mail.eml"))],
                    SIZE,
                );
                assert!(requested(&provider).is_empty());
            },
        );
    }

    #[test]
    fn moving_the_selection_keeps_matching_neighbors_and_drops_the_rest() {
        crate::test_support::gtk_test(
            "ui::preview::tests::preload::moving_the_selection_keeps_matching_neighbors_and_drops_the_rest",
            || {
                let (drawer, provider) = drawer(true);
                let state = &drawer.state;
                state
                    .preload
                    .reconcile(state, [Some(entry("a.png")), Some(entry("b.png"))], SIZE);
                state
                    .preload
                    .reconcile(state, [Some(entry("b.png")), Some(entry("c.png"))], SIZE);
                assert_eq!(requested(&provider), ["b.png", "a.png", "c.png"]);
                assert_eq!(*provider.dropped.borrow(), ["a.png"]);
                assert_eq!(state.preload.slot_names(), ["b.png", "c.png"]);
            },
        );
    }

    #[test]
    fn a_new_presentation_size_replaces_the_neighbors() {
        crate::test_support::gtk_test(
            "ui::preview::tests::preload::a_new_presentation_size_replaces_the_neighbors",
            || {
                let (drawer, provider) = drawer(true);
                let state = &drawer.state;
                let neighbors = || [Some(entry("a.png")), Some(entry("b.png"))];
                state.preload.reconcile(state, neighbors(), SIZE);
                let expanded = MediaPreviewSize::new(1280, 960);
                state.preload.reconcile(state, neighbors(), expanded);
                assert_eq!(provider.dropped.borrow().len(), 2);
                let preloads = provider.preloads.borrow();
                assert_eq!(preloads.len(), 4);
                assert!(
                    preloads[2..]
                        .iter()
                        .all(|pending| pending.request.media_size == expanded)
                );
            },
        );
    }

    #[test]
    fn finished_neighbors_are_ready_until_the_preference_is_turned_off() {
        crate::test_support::gtk_test(
            "ui::preview::tests::preload::finished_neighbors_are_ready_until_the_preference_is_turned_off",
            || {
                let (drawer, provider) = drawer(true);
                let state = &drawer.state;
                state
                    .preload
                    .reconcile(state, [Some(entry("a.png")), None], SIZE);
                assert!(
                    !state
                        .preload
                        .is_ready(&entry("a.png"), SIZE, PreviewDetail::Standard)
                );
                ready(&provider, "a.png", png());
                assert!(
                    state
                        .preload
                        .is_ready(&entry("a.png"), SIZE, PreviewDetail::Standard)
                );
                assert!(
                    !state.preload.is_ready(
                        &entry("a.png"),
                        MediaPreviewSize::new(100, 100),
                        PreviewDetail::Standard
                    ),
                    "a result for another size does not count"
                );
                state.preload.set_enabled(false);
                assert!(
                    !state
                        .preload
                        .is_ready(&entry("a.png"), SIZE, PreviewDetail::Standard)
                );
                assert_eq!(*provider.dropped.borrow(), ["a.png"]);
                assert!(state.preload.slot_names().is_empty());
            },
        );
    }

    #[test]
    fn a_failed_neighbor_is_not_ready() {
        crate::test_support::gtk_test(
            "ui::preview::tests::preload::a_failed_neighbor_is_not_ready",
            || {
                let (drawer, provider) = drawer(true);
                let state = &drawer.state;
                state
                    .preload
                    .reconcile(state, [Some(entry("a.png")), None], SIZE);
                let pending = provider.preloads.borrow();
                (pending[0].emit)(PreviewEvent::Failed {
                    request_id: pending[0].request.id,
                    entry: entry("a.png"),
                    message: "no".into(),
                });
                assert!(
                    !state
                        .preload
                        .is_ready(&entry("a.png"), SIZE, PreviewDetail::Standard)
                );
            },
        );
    }

    #[test]
    fn nothing_is_prepared_while_the_preference_is_off() {
        crate::test_support::gtk_test(
            "ui::preview::tests::preload::nothing_is_prepared_while_the_preference_is_off",
            || {
                let (drawer, provider) = drawer(false);
                let state = &drawer.state;
                state
                    .preload
                    .reconcile(state, [Some(entry("a.png")), Some(entry("b.png"))], SIZE);
                assert!(requested(&provider).is_empty());
                assert!(
                    !state
                        .preload
                        .is_ready(&entry("a.png"), SIZE, PreviewDetail::Standard)
                );
            },
        );
    }

    #[test]
    fn selections_in_quick_succession_are_not_treated_as_a_choice() {
        crate::test_support::gtk_test(
            "ui::preview::tests::preload::selections_in_quick_succession_are_not_treated_as_a_choice",
            || {
                let (drawer, _provider) = drawer(true);
                assert!(!drawer.state.preload.note_focus_change());
                assert!(drawer.state.preload.note_focus_change());
            },
        );
    }

    #[test]
    fn the_saved_preference_applies_at_startup_and_live_changes_reach_every_window() {
        crate::test_support::gtk_test(
            "ui::preview::tests::preload::the_saved_preference_applies_at_startup_and_live_changes_reach_every_window",
            || {
                let path = gtk::glib::user_config_dir().join("strata/settings.toml");
                std::fs::create_dir_all(path.parent().expect("settings directory"))
                    .expect("settings directory");
                std::fs::write(path, "preload_neighbor_previews = true\n").expect("seed settings");
                let manager = crate::ui::preferences::PreferenceManager::shared();
                assert!(manager.preload_neighbor_previews());
                let (first, first_provider) = drawer_with_saved_preference();
                let (second, _second_provider) = drawer_with_saved_preference();
                assert!(first.state.preload.is_enabled());
                assert!(second.state.preload.is_enabled());
                first
                    .state
                    .preload
                    .reconcile(&first.state, [Some(entry("a.png")), None], SIZE);
                manager.set_preload_neighbor_previews(false);
                let context = glib::MainContext::default();
                while context.iteration(false) {}
                assert!(!first.state.preload.is_enabled());
                assert!(!second.state.preload.is_enabled());
                assert_eq!(*first_provider.dropped.borrow(), ["a.png"]);
                manager.set_preload_neighbor_previews(true);
                while context.iteration(false) {}
                assert!(first.state.preload.is_enabled());
                assert!(second.state.preload.is_enabled());
            },
        );
    }

    #[test]
    fn a_parked_video_is_promoted_only_for_the_stream_it_decoded() {
        crate::test_support::gtk_test(
            "ui::preview::tests::preload::a_parked_video_is_promoted_only_for_the_stream_it_decoded",
            || {
                let (drawer, provider) = drawer(true);
                let state = &drawer.state;
                let clip = entry("clip.mp4");
                state
                    .preload
                    .reconcile(state, [None, Some(clip.clone())], SIZE);
                let source = SandboxedMedia {
                    path: "/unused".into(),
                    size: SIZE,
                    backend: MediaPreviewBackend::Software,
                    input_owner: None,
                };
                ready(
                    &provider,
                    "clip.mp4",
                    PreviewContent::SandboxedMedia {
                        media: source.clone(),
                    },
                );
                let media = state
                    .preload
                    .parked_stream("clip.mp4")
                    .expect("parked stream");
                media.use_test_loader(Rc::new(|source, start_tick| {
                    stream_preload(crate::media::Header {
                        width: source.size.width as u32,
                        height: source.size.height as u32,
                        audio: false,
                        duration_us: 3_600_000_000,
                        start_tick,
                        fps: 30,
                        native_fps: 30,
                    })
                }));
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                while !state.preload.is_ready(&clip, SIZE, PreviewDetail::Standard) {
                    assert!(std::time::Instant::now() < deadline, "first frame deadline");
                    glib::MainContext::default().iteration(false);
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                let elsewhere = SandboxedMedia {
                    path: "/other.mp4".into(),
                    ..source.clone()
                };
                assert!(state.preload.take_media(&elsewhere, &clip).is_none());
                let expanded = SandboxedMedia {
                    size: MediaPreviewSize::expanded(1920, 1080),
                    ..source.clone()
                };
                let promoted = state
                    .preload
                    .take_media(&expanded, &clip)
                    .expect("promoted at the size now asked for");
                assert!(!promoted.is_parked_ready());
                assert_eq!(promoted.requested_size(), Some(expanded.size));
                assert!(state.preload.slot_names().is_empty());
                promoted.close();
            },
        );
    }
    #[test]
    fn neighbors_decode_at_the_drawer_size_and_rate_even_while_expanded() {
        let size =
            super::super::preload::neighbor_media_size(MediaPreviewSize::expanded(3000, 2000));
        assert!(!size.expanded);
        assert_eq!(size.max_fps, 30);
        assert!(
            size.width <= MediaPreviewSize::MAX_EDGE && size.height <= MediaPreviewSize::MAX_EDGE
        );
    }
}
