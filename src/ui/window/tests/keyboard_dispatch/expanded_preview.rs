// SPDX-License-Identifier: MIT

use super::*;
use crate::{services::PreviewDetail, ui::preferences::ExpandedPreviewStyle};

const SHIFT: ModifierType = ModifierType::SHIFT_MASK;
const LONG_LINES: usize = 600;

/// Serves a picture for `.png`, a four-page document for `.pdf`, long wrapping text
/// for `wrap.txt`, and a long text document otherwise, and counts every load.
#[derive(Default)]
struct CountingPreview {
    loads: Cell<usize>,
    requests: RefCell<Vec<(String, PreviewDetail)>>,
}

impl CountingPreview {
    fn asked_for(&self, name: &str, expanded: bool) -> usize {
        self.requests
            .borrow()
            .iter()
            .filter(|(requested, detail)| requested == name && detail.is_expanded() == expanded)
            .count()
    }
}

/// Expanded requests get a raster twice as wide, as the sandbox would give.
fn picture_bytes(detail: PreviewDetail) -> Vec<u8> {
    let scale: usize = if detail.is_expanded() { 2 } else { 1 };
    let (width, height) = (400 * scale, 300 * scale);
    let pixels = glib::Bytes::from_owned(vec![180u8; width * height * 4]);
    gtk::gdk::MemoryTexture::new(
        width as i32,
        height as i32,
        gtk::gdk::MemoryFormat::R8g8b8a8,
        &pixels,
        width * 4,
    )
    .save_to_png_bytes()
    .to_vec()
}

impl PreviewProvider for CountingPreview {
    fn load(&self, request: PreviewRequest, emit: Rc<dyn Fn(PreviewEvent)>) -> LoadHandle {
        self.loads.set(self.loads.get() + 1);
        let name = request.entry.native_name.to_string_lossy().into_owned();
        self.requests
            .borrow_mut()
            .push((name.clone(), request.detail));
        glib::idle_add_local_once(move || {
            let content = if name.ends_with(".png") {
                PreviewContent::Rasterized {
                    png: picture_bytes(request.detail),
                }
            } else if name.ends_with(".pdf") {
                PreviewContent::Pdf {
                    png: picture_bytes(request.detail),
                    page: request.pdf_page,
                    pages: 4,
                    text_layer: None,
                }
            } else if name == "wrap.txt" {
                PreviewContent::Text {
                    content: (0..200)
                        .map(|line| format!("{line} {}\n", "word ".repeat(80)))
                        .collect(),
                    truncated: false,
                }
            } else {
                PreviewContent::Text {
                    content: (0..LONG_LINES)
                        .map(|line| format!("line {line}\n"))
                        .collect(),
                    truncated: false,
                }
            };
            emit(PreviewEvent::Ready(Preview {
                request_id: request.id,
                entry: request.entry,
                content_type: "text/plain".into(),
                content,
            }))
        });
        LoadHandle::new(|| {})
    }
}

struct Expanded {
    fixture: KeyboardFixture,
    provider: Rc<CountingPreview>,
}

impl Expanded {
    fn new() -> Self {
        Self::with_extra_files(&[])
    }

    fn with_extra_files(extra: &[&str]) -> Self {
        let provider = Rc::new(CountingPreview::default());
        let fixture = KeyboardFixture::with_provider(provider.clone());
        let root = fixture._directory.path();
        std::fs::create_dir(root.join("dir")).expect("folder");
        std::fs::write(root.join("package.deb"), b"!<arch>").expect("unsupported file");
        for name in extra {
            std::fs::write(root.join(name), b"content").expect("extra file");
        }
        PreferenceManager::shared().set_group_by_type(false);
        PreferenceManager::shared().set_expanded_preview_shift_controls(true);
        PreferenceManager::shared().set_expanded_preview_style(ExpandedPreviewStyle::Overlay);
        let browser = fixture.view.browser();
        fixture.preview.observe_browser(&browser);
        fixture.view.refresh();
        wait_loaded(&browser, 0);
        let total = 5 + extra.len();
        wait_until(|| entry_count(&browser) == total);
        browser.set_folders_first(0, true);
        browser.set_sort(0, SortKey::Name, SortDirection::Ascending);
        wait_until(|| {
            let names = source_names(&browser);
            names.len() == total && names[0] == "dir" && names[1..].is_sorted()
        });
        Self { fixture, provider }
    }

    fn select(&self, name: &str) {
        let browser = self.fixture.view.browser();
        let position = source_names(&browser)
            .iter()
            .position(|candidate| candidate == name)
            .expect("fixture entry");
        browser.select(0, position);
        self.fixture.view.browser().focus_active();
        wait_until(|| self.fixture.view.item_view_has_focus());
    }

    fn cursor_name(&self) -> String {
        self.fixture
            .view
            .browser()
            .focused_entry()
            .map(|entry| entry.display_name)
            .expect("focused entry")
    }

    fn expand_view(&self) {
        assert!(self.fixture.press(Key::space, SHIFT));
        assert!(self.fixture.preview.is_expanded());
    }

    fn expand(&self) {
        self.expand_view();
        wait_until(|| self.fixture.preview.document_scroll_value().is_some());
    }

    fn wait_document(&self) {
        let preview = &self.fixture.preview;
        wait_until(|| {
            preview.scroll_document(DocumentScroll::End);
            preview.document_scroll_value() > Some(0.0)
        });
        preview.scroll_document(DocumentScroll::Start);
    }

    fn settle_collapse(&self) {
        wait_until(|| !self.fixture.preview.is_expanded());
    }
}

fn window_keys(window: &gtk::Window) -> gtk::EventControllerKey {
    let controllers = window.observe_controllers();
    (0..controllers.n_items())
        .filter_map(|index| {
            controllers
                .item(index)
                .and_downcast::<gtk::EventControllerKey>()
        })
        .next()
        .expect("the fullscreen window has a key controller")
}

fn press_in(window: &gtk::Window, key: Key, modifiers: ModifierType) -> bool {
    window_keys(window).emit_by_name::<bool>("key-pressed", &[&key, &0u32, &modifiers])
}

fn expand_fullscreen(expanded: &Expanded) -> gtk::Window {
    PreferenceManager::shared().set_expanded_preview_style(ExpandedPreviewStyle::Fullscreen);
    expanded.select("a.txt");
    expanded.expand();
    expanded
        .fixture
        .preview
        .expanded_window()
        .expect("the fullscreen style opens a window")
}

fn drawer_of(preview: &PreviewDrawer) -> Option<gtk::Widget> {
    preview
        .pane_widget()
        .ancestor(gtk::Revealer::static_type())
        .and_then(|revealer| revealer.parent())
}

fn press_plain(expanded: &Expanded, key: Key) -> bool {
    expanded.fixture.press(key, ModifierType::empty())
}

#[test]
fn shift_space_expands_the_preview_and_escape_returns_to_the_drawer() {
    crate::test_support::gtk_test(
        "ui::window::tests::keyboard_dispatch::expanded_preview::shift_space_expands_the_preview_and_escape_returns_to_the_drawer",
        || {
            let expanded = Expanded::new();
            let preview = &expanded.fixture.preview;
            expanded.select("a.txt");

            expanded.expand();
            assert!(preview.is_enabled());
            let pane = preview.pane_widget();
            let overlay_root = pane.ancestor(gtk::Overlay::static_type());
            assert!(
                overlay_root.is_some_and(|overlay| overlay == expanded.fixture.overlay),
                "the pane sits in the window overlay"
            );

            assert!(press_plain(&expanded, Key::Escape));
            expanded.settle_collapse();
            assert!(preview.is_enabled(), "Escape leaves the small preview open");
            assert_eq!(
                drawer_of(preview),
                Some(preview.widget()),
                "the pane returned to the drawer"
            );

            assert!(press_plain(&expanded, Key::Escape));
            assert!(!preview.is_enabled(), "a second Escape closes the preview");
        },
    );
}

#[test]
fn expanding_and_collapsing_reuses_the_same_live_preview() {
    crate::test_support::gtk_test(
        "ui::window::tests::keyboard_dispatch::expanded_preview::expanding_and_collapsing_reuses_the_same_live_preview",
        || {
            let expanded = Expanded::new();
            let preview = &expanded.fixture.preview;
            expanded.select("a.txt");
            assert!(press_plain(&expanded, Key::space));
            expanded.wait_document();
            preview.scroll_document(DocumentScroll::Page(1));
            let (pane, content) = (preview.pane_widget(), preview.content_widget());
            let scroll = preview.document_scroll_value();
            let loads = expanded.provider.loads.get();
            assert!(scroll.is_some_and(|scroll| scroll > 0.0));

            expanded.expand();
            assert_eq!(preview.pane_widget(), pane);
            assert_eq!(preview.content_widget(), content);
            wait_until(|| preview.pane_widget().is_mapped());
            assert!(press_plain(&expanded, Key::Escape));
            expanded.settle_collapse();

            assert_eq!(preview.pane_widget(), pane);
            assert_eq!(preview.content_widget(), content);
            assert_eq!(expanded.provider.loads.get(), loads, "nothing was reloaded");
            assert!(preview.is_enabled(), "expanding never stopped the preview");
        },
    );
}

#[test]
fn shift_space_opens_a_closed_preview_straight_into_the_expanded_view() {
    crate::test_support::gtk_test(
        "ui::window::tests::keyboard_dispatch::expanded_preview::shift_space_opens_a_closed_preview_straight_into_the_expanded_view",
        || {
            let expanded = Expanded::new();
            expanded.select("b.txt");
            assert!(!expanded.fixture.preview.is_enabled());
            expanded.expand();
            assert!(expanded.fixture.preview.is_enabled());
            wait_until(|| expanded.provider.loads.get() == 1);

            assert!(press_plain(&expanded, Key::Escape));
            expanded.settle_collapse();
            assert!(expanded.fixture.preview.is_enabled());
        },
    );
}

#[test]
fn shift_space_is_left_to_text_fields_and_to_folders() {
    crate::test_support::gtk_test(
        "ui::window::tests::keyboard_dispatch::expanded_preview::shift_space_is_left_to_text_fields_and_to_folders",
        || {
            let expanded = Expanded::new();
            let preview = &expanded.fixture.preview;

            expanded.select("dir");
            assert!(expanded.fixture.press(Key::space, SHIFT));
            assert!(!preview.is_expanded(), "a folder has nothing to expand");
            assert!(!preview.is_enabled());

            expanded.select("a.txt");
            assert!(expanded.fixture.press(Key::f, ModifierType::CONTROL_MASK));
            wait_until(|| expanded.fixture.view.filter_has_focus());
            assert!(
                !expanded.fixture.press(Key::space, SHIFT),
                "the filter field keeps typing a space"
            );
            assert!(!preview.is_expanded());
            assert!(!preview.is_enabled());
        },
    );
}

#[test]
fn plain_arrows_change_file_and_shift_arrows_drive_the_preview() {
    crate::test_support::gtk_test(
        "ui::window::tests::keyboard_dispatch::expanded_preview::plain_arrows_change_file_and_shift_arrows_drive_the_preview",
        || {
            let expanded = Expanded::new();
            let preferences = PreferenceManager::shared();
            let preview = &expanded.fixture.preview;
            expanded.select("a.txt");
            expanded.expand();
            expanded.wait_document();

            for hold_shift in [true, false] {
                preferences.set_expanded_preview_shift_controls(hold_shift);
                let navigate = if hold_shift {
                    ModifierType::empty()
                } else {
                    SHIFT
                };
                let control = if hold_shift {
                    SHIFT
                } else {
                    ModifierType::empty()
                };

                expanded.wait_document();
                let before = preview.document_scroll_value().expect("document");
                assert!(expanded.fixture.press(Key::Down, control));
                wait_until(|| preview.document_scroll_value() > Some(before));
                assert_eq!(expanded.cursor_name(), "a.txt", "control must not move");
                assert!(expanded.fixture.press(Key::Up, control));
                wait_until(|| preview.document_scroll_value() == Some(before));

                assert!(expanded.fixture.press(Key::Down, navigate));
                assert_eq!(expanded.cursor_name(), "b.txt");
                assert!(expanded.fixture.press(Key::Up, navigate));
                assert_eq!(expanded.cursor_name(), "a.txt");
                assert!(preview.is_expanded(), "changing file keeps the view open");
            }
        },
    );
}

#[test]
fn changing_file_skips_folders_and_unsupported_files_and_stops_at_the_ends() {
    crate::test_support::gtk_test(
        "ui::window::tests::keyboard_dispatch::expanded_preview::changing_file_skips_folders_and_unsupported_files_and_stops_at_the_ends",
        || {
            let expanded = Expanded::new();
            expanded.select("a.txt");
            expanded.expand();

            assert!(expanded.fixture.press(Key::Up, ModifierType::empty()));
            assert_eq!(expanded.cursor_name(), "a.txt", "the folder is skipped");
            for name in ["b.txt", "c.txt"] {
                assert!(expanded.fixture.press(Key::Right, ModifierType::empty()));
                assert_eq!(expanded.cursor_name(), name);
            }
            assert!(expanded.fixture.press(Key::Down, ModifierType::empty()));
            assert_eq!(
                expanded.cursor_name(),
                "c.txt",
                "package.deb has no preview and the list ends"
            );
        },
    );
}

#[test]
fn space_closes_a_document_preview_and_shift_space_collapses_it() {
    crate::test_support::gtk_test(
        "ui::window::tests::keyboard_dispatch::expanded_preview::space_closes_a_document_preview_and_shift_space_collapses_it",
        || {
            let expanded = Expanded::new();
            let preview = &expanded.fixture.preview;
            expanded.select("a.txt");
            expanded.expand();
            assert!(expanded.fixture.press(Key::space, SHIFT));
            expanded.settle_collapse();
            assert!(preview.is_enabled());

            expanded.expand();
            assert!(press_plain(&expanded, Key::space));
            expanded.settle_collapse();
            assert!(!preview.is_enabled(), "Space closes a non-media preview");
        },
    );
}

#[test]
fn the_expanded_view_swallows_keys_meant_for_the_hidden_listing() {
    crate::test_support::gtk_test(
        "ui::window::tests::keyboard_dispatch::expanded_preview::the_expanded_view_swallows_keys_meant_for_the_hidden_listing",
        || {
            let expanded = Expanded::new();
            expanded.select("a.txt");
            expanded.expand();
            for key in [Key::a, Key::Delete, Key::F2, Key::BackSpace, Key::Return] {
                assert!(press_plain(&expanded, key), "{key:?}");
            }
            assert_eq!(expanded.cursor_name(), "a.txt");
            assert!(!expanded.fixture.view.filter_has_focus());
            assert!(!expanded.fixture.view.rename_is_active());
            assert!(expanded.fixture.preview.is_expanded());
        },
    );
}

#[test]
fn closing_the_window_while_expanded_tears_the_view_down() {
    crate::test_support::gtk_test(
        "ui::window::tests::keyboard_dispatch::expanded_preview::closing_the_window_while_expanded_tears_the_view_down",
        || {
            let expanded = Expanded::new();
            expanded.select("a.txt");
            expanded.expand();
            expanded.fixture.window.destroy();
            assert!(!expanded.fixture.preview.is_expanded());
            assert!(!expanded.fixture.preview.is_enabled());
        },
    );
}

#[test]
fn drawers_without_expansion_ignore_the_request() {
    crate::test_support::gtk_test(
        "ui::window::tests::keyboard_dispatch::expanded_preview::drawers_without_expansion_ignore_the_request",
        || {
            let drawer = PreviewDrawer::new(Rc::new(CountingPreview::default()), false);
            assert!(!drawer.expand(None));
            assert!(!drawer.is_expanded());
        },
    );
}

#[test]
fn the_fullscreen_style_moves_the_live_pane_into_its_own_window() {
    crate::test_support::gtk_test(
        "ui::window::tests::keyboard_dispatch::expanded_preview::the_fullscreen_style_moves_the_live_pane_into_its_own_window",
        || {
            let expanded = Expanded::new();
            let preview = &expanded.fixture.preview;
            expanded.select("a.txt");
            assert!(press_plain(&expanded, Key::space));
            expanded.wait_document();
            let (pane, content) = (preview.pane_widget(), preview.content_widget());
            let loads = expanded.provider.loads.get();

            let window = expand_fullscreen(&expanded);
            assert_eq!(preview.pane_widget(), pane);
            assert_eq!(
                pane.root().and_downcast::<gtk::Window>().as_ref(),
                Some(&window)
            );
            assert_ne!(&window, expanded.fixture.window.upcast_ref::<gtk::Window>());
            assert!(preview.is_enabled());

            assert!(press_in(&window, Key::Escape, ModifierType::empty()));
            assert!(!preview.is_expanded());
            assert_eq!(
                drawer_of(preview),
                Some(preview.widget()),
                "the pane returned to the drawer"
            );
            assert_eq!(preview.content_widget(), content);
            assert_eq!(expanded.provider.loads.get(), loads, "nothing was reloaded");
            assert!(preview.is_enabled(), "collapsing leaves the small preview");
            assert!(
                !gtk::Window::list_toplevels()
                    .iter()
                    .any(|toplevel| toplevel == window.upcast_ref::<gtk::Widget>()),
                "the fullscreen window is gone"
            );
        },
    );
}

#[test]
fn the_fullscreen_window_takes_the_same_keys_as_the_overlay() {
    crate::test_support::gtk_test(
        "ui::window::tests::keyboard_dispatch::expanded_preview::the_fullscreen_window_takes_the_same_keys_as_the_overlay",
        || {
            let expanded = Expanded::new();
            let window = expand_fullscreen(&expanded);
            let none = ModifierType::empty();

            assert!(press_in(&window, Key::Down, none));
            assert_eq!(expanded.cursor_name(), "b.txt", "plain arrows change file");
            assert!(press_in(&window, Key::Up, none));
            assert_eq!(expanded.cursor_name(), "a.txt");
            assert!(press_in(&window, Key::a, none), "typing is swallowed");
            assert!(expanded.fixture.preview.is_expanded());

            assert!(press_in(&window, Key::space, SHIFT));
            assert!(!expanded.fixture.preview.is_expanded());
        },
    );
}

#[test]
fn closing_the_fullscreen_window_returns_to_the_drawer() {
    crate::test_support::gtk_test(
        "ui::window::tests::keyboard_dispatch::expanded_preview::closing_the_fullscreen_window_returns_to_the_drawer",
        || {
            let expanded = Expanded::new();
            let window = expand_fullscreen(&expanded);
            window.close();
            assert!(!expanded.fixture.preview.is_expanded());
            assert!(expanded.fixture.preview.is_enabled());
            assert_eq!(
                drawer_of(&expanded.fixture.preview),
                Some(expanded.fixture.preview.widget())
            );
        },
    );
}

#[test]
fn the_button_and_scrim_leave_the_expanded_view() {
    crate::test_support::gtk_test(
        "ui::window::tests::keyboard_dispatch::expanded_preview::the_button_and_scrim_leave_the_expanded_view",
        || {
            let expanded = Expanded::new();
            let preview = &expanded.fixture.preview;
            expanded.select("a.txt");
            expanded.expand();
            preview.toggle_expanded_from_button();
            expanded.settle_collapse();
            assert!(preview.is_enabled());

            preview.toggle_expanded_from_button();
            assert!(preview.is_expanded(), "the button expands the open preview");
        },
    );
}

#[test]
fn closing_the_main_window_takes_the_fullscreen_window_with_it() {
    crate::test_support::gtk_test(
        "ui::window::tests::keyboard_dispatch::expanded_preview::closing_the_main_window_takes_the_fullscreen_window_with_it",
        || {
            let expanded = Expanded::new();
            let window = expand_fullscreen(&expanded);
            expanded.fixture.window.destroy();
            assert!(!expanded.fixture.preview.is_expanded());
            assert!(
                !gtk::Window::list_toplevels()
                    .iter()
                    .any(|toplevel| toplevel == window.upcast_ref::<gtk::Widget>()),
                "no orphaned fullscreen window"
            );
        },
    );
}

fn media_fixture() -> Expanded {
    Expanded::with_extra_files(&["doc.pdf", "image.png", "wrap.txt"])
}

fn wait_view(expanded: &Expanded, ready: impl Fn(&PreviewDrawer) -> bool) {
    let preview = &expanded.fixture.preview;
    wait_until(|| ready(preview));
}

fn press_shift(expanded: &Expanded, key: Key) -> bool {
    expanded.fixture.press(key, SHIFT)
}

#[test]
fn images_zoom_and_pan_only_while_expanded_and_keep_their_view() {
    crate::test_support::gtk_test(
        "ui::window::tests::keyboard_dispatch::expanded_preview::images_zoom_and_pan_only_while_expanded_and_keep_their_view",
        || {
            let expanded = media_fixture();
            let preview = &expanded.fixture.preview;
            expanded.select("image.png");
            assert!(press_plain(&expanded, Key::space));
            wait_view(&expanded, |preview| preview.image_zoom().is_some());
            assert!(
                !preview.image_is_zoomed(),
                "the drawer shows the whole image"
            );

            expanded.expand_view();
            wait_until(|| preview.pane_widget().is_mapped());
            assert!(
                press_shift(&expanded, Key::Left),
                "at fit, control arrows change file"
            );
            assert_eq!(expanded.cursor_name(), "doc.pdf");
            assert!(press_shift(&expanded, Key::Right));
            assert_eq!(expanded.cursor_name(), "image.png");
            wait_view(&expanded, |preview| preview.image_zoom().is_some());

            for _ in 0..3 {
                assert!(expanded.fixture.press(Key::plus, SHIFT));
            }
            assert!(preview.image_zoom().is_some_and(|zoom| zoom > 1.9));
            assert!(preview.image_is_zoomed());

            wait_view(&expanded, |preview| preview.image_is_laid_out());
            let before = preview.image_center().expect("centre");
            assert!(
                press_shift(&expanded, Key::Right),
                "control arrows pan a zoomed image"
            );
            let after = preview.image_center().expect("centre");
            assert!(
                after.0 > before.0,
                "the view moved right: {before:?} -> {after:?}"
            );
            assert_eq!(
                expanded.cursor_name(),
                "image.png",
                "panning does not change file"
            );

            assert!(press_plain(&expanded, Key::Escape));
            expanded.settle_collapse();
            assert!(
                !preview.image_is_zoomed(),
                "the drawer always fits the image"
            );
            assert!(
                preview.image_zoom().is_some_and(|zoom| zoom > 1.9),
                "the zoom is kept"
            );

            expanded.expand_view();
            assert!(
                preview.image_is_zoomed(),
                "the same zoom returns when expanded again"
            );
            assert!(expanded.fixture.press(Key::_0, ModifierType::empty()));
            assert_eq!(preview.image_zoom(), Some(1.0));
            assert!(expanded.fixture.press(Key::minus, ModifierType::empty()));
            assert_eq!(preview.image_zoom(), Some(1.0), "zoom never goes below fit");
        },
    );
}

#[test]
fn pdf_keys_zoom_and_turn_pages() {
    crate::test_support::gtk_test(
        "ui::window::tests::keyboard_dispatch::expanded_preview::pdf_keys_zoom_and_turn_pages",
        || {
            let expanded = media_fixture();
            let preview = &expanded.fixture.preview;
            expanded.select("doc.pdf");
            assert!(press_plain(&expanded, Key::space));
            wait_view(&expanded, |preview| preview.pdf_zoom().is_some());
            expanded.expand_view();
            wait_until(|| {
                preview
                    .document_position()
                    .is_some_and(|(_, extent)| extent > 1000.0)
            });

            assert!(expanded.fixture.press(Key::plus, SHIFT));
            assert!(preview.pdf_zoom().is_some_and(|zoom| zoom > 1.2));
            assert!(expanded.fixture.press(Key::_0, ModifierType::empty()));
            assert_eq!(preview.pdf_zoom(), Some(1.0));
            assert!(expanded.fixture.press(Key::minus, ModifierType::empty()));
            assert_eq!(preview.pdf_zoom(), Some(1.0), "zoom never goes below fit");
            for _ in 0..12 {
                expanded.fixture.press(Key::plus, SHIFT);
            }
            assert!(preview.pdf_zoom().is_some_and(|zoom| zoom <= 4.0));
            expanded.fixture.press(Key::_0, ModifierType::empty());

            let top = preview.document_position().expect("position").0;
            assert!(
                press_shift(&expanded, Key::Right),
                "Shift+Right turns to the next page"
            );
            let (next, _) = preview.document_position().expect("position");
            assert!(next > top, "the next page is further down: {top} -> {next}");
            assert_eq!(expanded.cursor_name(), "doc.pdf");
            assert!(press_shift(&expanded, Key::Left), "Shift+Left turns back");
            let (back, _) = preview.document_position().expect("position");
            assert!(
                (back - top).abs() < 0.01,
                "back on the first page: {top} -> {back}"
            );
        },
    );
}

#[test]
fn a_document_keeps_its_place_when_the_text_reflows_to_a_new_width() {
    crate::test_support::gtk_test(
        "ui::window::tests::keyboard_dispatch::expanded_preview::a_document_keeps_its_place_when_the_text_reflows_to_a_new_width",
        || {
            let expanded = media_fixture();
            let preview = &expanded.fixture.preview;
            PreferenceManager::shared().set_preview_text_wrap(true);
            expanded.select("wrap.txt");
            assert!(press_plain(&expanded, Key::space));
            wait_until(|| {
                preview
                    .document_position()
                    .is_some_and(|(_, extent)| extent > 1000.0)
            });
            preview.set_document_fraction(0.5);
            let (fraction, small_extent) = preview.document_position().expect("position");
            assert!((fraction - 0.5).abs() < 0.02, "{fraction}");

            expanded.expand();
            wait_until(|| {
                preview
                    .document_position()
                    .is_some_and(|(_, extent)| (extent - small_extent).abs() > 1.0)
            });
            wait_until(|| {
                preview
                    .document_position()
                    .is_some_and(|(fraction, _)| (fraction - 0.5).abs() < 0.03)
            });

            assert!(press_plain(&expanded, Key::Escape));
            expanded.settle_collapse();
        },
    );
}

fn enable_motion() {
    PreferenceManager::shared().set_reduce_motion(false);
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_enable_animations(true);
    }
}

/// Runs the main loop for a while, checking the line at the middle of the view
/// never leaves `line`. Returns how far the document's height moved.
fn watch_reading_place(preview: &PreviewDrawer, line: i32, milliseconds: u64) -> f64 {
    let deadline = Instant::now() + Duration::from_millis(milliseconds);
    let (_, start) = preview.document_position().expect("position");
    let mut travelled = 0.0f64;
    while Instant::now() < deadline {
        glib::MainContext::default().iteration(false);
        if let (Some(middle), Some((_, extent))) =
            (preview.text_center_line(), preview.document_position())
        {
            travelled = travelled.max((extent - start).abs());
            assert!(
                (middle - line).abs() <= 2,
                "the reading place moved from line {line} to {middle} while the card moved"
            );
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    travelled
}

#[test]
fn a_document_keeps_its_place_while_the_card_grows_and_shrinks() {
    crate::test_support::gtk_test(
        "ui::window::tests::keyboard_dispatch::expanded_preview::a_document_keeps_its_place_while_the_card_grows_and_shrinks",
        || {
            let expanded = media_fixture();
            enable_motion();
            let preview = &expanded.fixture.preview;
            PreferenceManager::shared().set_preview_text_wrap(true);
            expanded.select("wrap.txt");
            assert!(press_plain(&expanded, Key::space));
            wait_until(|| {
                preview
                    .document_position()
                    .is_some_and(|(_, extent)| extent > 1000.0)
            });
            preview.set_document_fraction(0.5);
            let line = preview.text_center_line().expect("a text preview");

            expanded.expand_view();
            assert!(
                watch_reading_place(preview, line, 900) > 1.0,
                "the text reflowed to the card's width"
            );

            assert!(press_plain(&expanded, Key::Escape));
            assert!(preview.is_collapsing(), "the card animates back");
            assert!(watch_reading_place(preview, line, 900) > 1.0);
            assert!(!preview.is_expanded());
        },
    );
}

#[test]
fn a_card_that_is_sent_back_mid_animation_returns_to_the_drawer_untouched() {
    crate::test_support::gtk_test(
        "ui::window::tests::keyboard_dispatch::expanded_preview::a_card_that_is_sent_back_mid_animation_returns_to_the_drawer_untouched",
        || {
            let expanded = media_fixture();
            enable_motion();
            let preview = &expanded.fixture.preview;
            expanded.select("a.txt");
            assert!(press_plain(&expanded, Key::space));
            expanded.wait_document();
            let (pane, content) = (preview.pane_widget(), preview.content_widget());
            let loads = expanded.provider.loads.get();

            expanded.expand_view();
            wait_until(|| {
                preview
                    .card_progress()
                    .is_some_and(|progress| progress > 0.02)
            });
            assert!(press_plain(&expanded, Key::Escape));
            assert!(preview.is_collapsing(), "Escape reverses the opening card");
            assert!(
                expanded.fixture.press(Key::space, SHIFT),
                "keys are held back until the card is home"
            );
            assert!(preview.is_collapsing());
            expanded.settle_collapse();

            assert!(!preview.is_collapsing());
            assert_eq!(
                drawer_of(preview),
                Some(preview.widget()),
                "the pane is back in the drawer"
            );
            assert_eq!(preview.pane_widget(), pane);
            assert_eq!(preview.content_widget(), content);
            assert_eq!(expanded.provider.loads.get(), loads, "nothing was reloaded");

            expanded.select("a.txt");
            expanded.expand_view();
            assert!(press_plain(&expanded, Key::Escape));
            expanded.settle_collapse();
        },
    );
}

#[test]
fn expanding_an_image_swaps_in_a_larger_raster_and_keeps_zoom_and_place() {
    crate::test_support::gtk_test(
        "ui::window::tests::keyboard_dispatch::expanded_preview::expanding_an_image_swaps_in_a_larger_raster_and_keeps_zoom_and_place",
        || {
            let expanded = media_fixture();
            let preview = &expanded.fixture.preview;
            expanded.select("image.png");
            assert!(press_plain(&expanded, Key::space));
            wait_view(&expanded, |preview| preview.image_zoom().is_some());
            assert_eq!(preview.image_texture_size(), Some((400, 300)));
            assert_eq!(expanded.provider.asked_for("image.png", true), 0);
            let content = preview.content_widget();

            expanded.expand_view();
            for _ in 0..3 {
                assert!(expanded.fixture.press(Key::plus, SHIFT));
            }
            wait_view(&expanded, |preview| {
                preview.image_texture_size() == Some((800, 600))
            });
            assert_eq!(expanded.provider.asked_for("image.png", true), 1);
            assert!(
                preview.image_zoom().is_some_and(|zoom| zoom > 1.9),
                "zoom survives the swap"
            );
            assert_eq!(
                preview.content_widget(),
                content,
                "the picture was not rebuilt"
            );

            assert!(press_plain(&expanded, Key::Escape));
            expanded.settle_collapse();
            assert_eq!(
                preview.image_texture_size(),
                Some((800, 600)),
                "collapsing keeps the sharper raster"
            );
            assert_eq!(expanded.provider.asked_for("image.png", false), 1);
            assert_eq!(expanded.provider.asked_for("image.png", true), 1);
        },
    );
}

#[test]
fn a_file_opened_while_expanded_loads_at_the_larger_size_straight_away() {
    crate::test_support::gtk_test(
        "ui::window::tests::keyboard_dispatch::expanded_preview::a_file_opened_while_expanded_loads_at_the_larger_size_straight_away",
        || {
            let expanded = media_fixture();
            expanded.select("doc.pdf");
            expanded.expand_view();
            wait_view(&expanded, |preview| preview.pdf_zoom().is_some());
            assert!(
                press_plain(&expanded, Key::Right),
                "plain arrows change file"
            );
            assert_eq!(expanded.cursor_name(), "image.png");
            wait_view(&expanded, |preview| {
                preview.image_texture_size() == Some((800, 600))
            });
            assert_eq!(expanded.provider.asked_for("image.png", false), 0);
            assert_eq!(
                expanded.provider.asked_for("image.png", true),
                1,
                "one request, made at the expanded size"
            );
        },
    );
}

#[test]
fn pdf_pages_on_screen_sharpen_in_place_when_expanded() {
    crate::test_support::gtk_test(
        "ui::window::tests::keyboard_dispatch::expanded_preview::pdf_pages_on_screen_sharpen_in_place_when_expanded",
        || {
            let expanded = media_fixture();
            let preview = &expanded.fixture.preview;
            expanded.select("doc.pdf");
            assert!(press_plain(&expanded, Key::space));
            wait_view(&expanded, |preview| {
                preview.pdf_page_texture_width() == Some(400)
            });
            assert_eq!(expanded.provider.asked_for("doc.pdf", true), 0);
            let content = preview.content_widget();
            let loads = expanded.provider.loads.get();

            expanded.expand_view();
            wait_view(&expanded, |preview| {
                preview.pdf_page_texture_width() == Some(800)
            });
            assert!(expanded.provider.asked_for("doc.pdf", true) >= 1);
            assert_eq!(
                preview.content_widget(),
                content,
                "the viewer was not rebuilt"
            );
            assert!(expanded.provider.loads.get() > loads);

            let sharpened = expanded.provider.asked_for("doc.pdf", true);
            assert!(press_plain(&expanded, Key::Escape));
            expanded.settle_collapse();
            spin_for(300);
            assert_eq!(
                expanded.provider.asked_for("doc.pdf", true),
                sharpened,
                "collapsing makes no more large requests"
            );
        },
    );
}

fn spin_for(milliseconds: u64) {
    let end = Instant::now() + Duration::from_millis(milliseconds);
    while Instant::now() < end {
        glib::MainContext::default().iteration(false);
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn tab_cycles_through_the_expanded_view_and_never_reaches_the_window_behind() {
    crate::test_support::gtk_test(
        "ui::window::tests::keyboard_dispatch::expanded_preview::tab_cycles_through_the_expanded_view_and_never_reaches_the_window_behind",
        || {
            let expanded = Expanded::new();
            let preview = &expanded.fixture.preview;
            expanded.select("a.txt");
            expanded.expand();
            for step in 0..14 {
                let (key, modifiers) = if step % 2 == 0 {
                    (Key::Tab, ModifierType::empty())
                } else {
                    (Key::ISO_Left_Tab, SHIFT)
                };
                assert!(expanded.fixture.press(key, modifiers));
                assert!(
                    preview.owns_focus(preview.focused_widget().as_ref()),
                    "step {step}: focus left the expanded view"
                );
            }
            for _ in 0..14 {
                assert!(expanded.fixture.press(Key::Tab, ModifierType::empty()));
                assert!(preview.owns_focus(preview.focused_widget().as_ref()));
            }
        },
    );
}

#[test]
fn a_zoomed_pdf_pans_sideways_instead_of_turning_the_page() {
    crate::test_support::gtk_test(
        "ui::window::tests::keyboard_dispatch::expanded_preview::a_zoomed_pdf_pans_sideways_instead_of_turning_the_page",
        || {
            let expanded = media_fixture();
            let preview = &expanded.fixture.preview;
            expanded.select("doc.pdf");
            assert!(press_plain(&expanded, Key::space));
            wait_view(&expanded, |preview| preview.pdf_zoom().is_some());
            expanded.expand_view();
            wait_until(|| {
                preview
                    .document_position()
                    .is_some_and(|(_, extent)| extent > 1000.0)
            });

            assert!(expanded.fixture.press(Key::plus, SHIFT));
            wait_until(|| {
                preview
                    .document_pan()
                    .is_some_and(|(_, range)| range > 20.0)
            });
            spin_for(200);
            let (fraction, _) = preview.document_position().expect("position");
            let (pan, _) = preview.document_pan().expect("pan");

            assert!(press_shift(&expanded, Key::Right));
            let (after_pan, _) = preview.document_pan().expect("pan");
            let (after_fraction, _) = preview.document_position().expect("position");
            assert!(
                after_pan > pan,
                "the page moved sideways: {pan} -> {after_pan}"
            );
            assert!(
                (after_fraction - fraction).abs() < 0.002,
                "and did not turn: {fraction} -> {after_fraction}"
            );
            assert_eq!(expanded.cursor_name(), "doc.pdf");
        },
    );
}
