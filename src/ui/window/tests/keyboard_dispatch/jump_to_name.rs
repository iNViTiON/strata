// SPDX-License-Identifier: MIT

use std::time::Duration;

use super::*;
use crate::ui::preferences::TypingMode;

const NONE: ModifierType = ModifierType::empty();
const CONTROL: ModifierType = ModifierType::CONTROL_MASK;

/// The a-names list as a.txt, apple.txt, apricot.txt, avocado.txt whether
/// sorted by name or by age. The other names each have a unique first letter,
/// and `docs` holds one file.
fn seed_jump_names(fixture: &KeyboardFixture) {
    for name in [
        "apple.txt",
        "apricot.txt",
        "avocado.txt",
        "banana.txt",
        "Zebra.md",
        "ärger.md",
    ] {
        std::fs::write(fixture._directory.path().join(name), b"jump").expect("fixture file");
    }
    let docs = fixture._directory.path().join("docs");
    std::fs::create_dir(&docs).expect("docs folder");
    std::fs::write(docs.join("inside.txt"), b"jump").expect("docs file");
    fixture.view.refresh();
    let browser = fixture.view.browser();
    wait_loaded(&browser, 0);
    wait_until(|| entry_count(&browser) == 10);
}

fn jump_fixture(mode: TypingMode) -> KeyboardFixture {
    let fixture = KeyboardFixture::new();
    seed_jump_names(&fixture);
    let preferences = PreferenceManager::shared();
    preferences.set_type_to_search(false);
    preferences.set_typing_mode(mode);
    fixture
        .view
        .set_jump_to_name_idle_timeout(Duration::from_secs(60));
    fixture
}

fn show_mode(fixture: &KeyboardFixture, mode: BrowserMode, name: &str) {
    fixture.view.set_view_mode(mode);
    pump(300);
    select_named(fixture, name);
}

fn type_key(fixture: &KeyboardFixture, key: Key) {
    assert!(fixture.press(key, NONE), "{key:?} is claimed by jump mode");
}

fn press_control(fixture: &KeyboardFixture, key: Key) {
    assert!(fixture.press(key, CONTROL), "Ctrl+{key:?} is claimed");
}

#[test]
fn jump_to_name_selects_prefix_matches_in_every_view() {
    crate::test_support::gtk_test(
        "ui::window::tests::keyboard_dispatch::jump_to_name::jump_to_name_selects_prefix_matches_in_every_view",
        || {
            let fixture = jump_fixture(TypingMode::JumpToName);
            let browser = fixture.view.browser();
            for mode in [BrowserMode::Columns, BrowserMode::List, BrowserMode::Icons] {
                show_mode(&fixture, mode, "c.txt");
                assert!(!fixture.view.jump_to_name_active(), "{mode:?}");

                type_key(&fixture, Key::a);
                assert!(fixture.view.jump_to_name_active(), "{mode:?}");
                assert_eq!(focused_name(&browser), "a.txt", "{mode:?}: first match");
                type_key(&fixture, Key::p);
                assert_eq!(focused_name(&browser), "apple.txt", "{mode:?}");
                type_key(&fixture, Key::r);
                assert_eq!(focused_name(&browser), "apricot.txt", "{mode:?}");
                assert_eq!(fixture.view.jump_to_name_prefix(), "apr");

                type_key(&fixture, Key::x);
                assert_eq!(
                    focused_name(&browser),
                    "apricot.txt",
                    "{mode:?}: a miss keeps the selection"
                );
                assert_eq!(fixture.view.jump_to_name_prefix(), "aprx");
                assert!(!fixture.view.jump_to_name_matched(), "{mode:?}");

                type_key(&fixture, Key::BackSpace);
                assert!(fixture.view.jump_to_name_matched(), "{mode:?}");
                type_key(&fixture, Key::BackSpace);
                assert_eq!(focused_name(&browser), "apple.txt", "{mode:?}");
                type_key(&fixture, Key::BackSpace);
                assert_eq!(focused_name(&browser), "a.txt", "{mode:?}");

                type_key(&fixture, Key::a);
                assert_eq!(fixture.view.jump_to_name_prefix(), "a", "{mode:?}");
                assert_eq!(focused_name(&browser), "apple.txt", "{mode:?}: same letter");
                for expected in ["apricot.txt", "avocado.txt", "a.txt"] {
                    press_control(&fixture, Key::n);
                    assert_eq!(focused_name(&browser), expected, "{mode:?}: Ctrl+N");
                }
                press_control(&fixture, Key::p);
                assert_eq!(
                    focused_name(&browser),
                    "avocado.txt",
                    "{mode:?}: Ctrl+P wraps"
                );

                assert!(fixture.press(Key::Escape, NONE), "{mode:?}");
                assert!(!fixture.view.jump_to_name_active(), "{mode:?}");
                assert_eq!(focused_name(&browser), "avocado.txt", "{mode:?}: Esc keeps");
                assert_eq!(fixture.selected().len(), 1, "{mode:?}");

                type_key(&fixture, Key::z);
                assert_eq!(focused_name(&browser), "Zebra.md", "{mode:?}: case");
                assert!(fixture.press(Key::Escape, NONE));
                type_key(&fixture, Key::adiaeresis);
                assert_eq!(focused_name(&browser), "ärger.md", "{mode:?}: Unicode");
                assert!(fixture.press(Key::Escape, NONE));
            }
        },
    );
}

#[test]
fn jump_to_name_keys_end_the_session_and_keep_their_normal_meaning() {
    crate::test_support::gtk_test(
        "ui::window::tests::keyboard_dispatch::jump_to_name::jump_to_name_keys_end_the_session_and_keep_their_normal_meaning",
        || {
            let fixture = jump_fixture(TypingMode::JumpToName);
            let browser = fixture.view.browser();
            for mode in [BrowserMode::Columns, BrowserMode::List, BrowserMode::Icons] {
                show_mode(&fixture, mode, "c.txt");

                type_key(&fixture, Key::a);
                assert_eq!(focused_name(&browser), "a.txt", "{mode:?}");
                fixture.press(Key::Down, NONE);
                assert!(
                    !fixture.view.jump_to_name_active(),
                    "{mode:?}: arrows end it"
                );
                if mode == BrowserMode::Columns {
                    assert_ne!(focused_name(&browser), "a.txt", "{mode:?}: and move");
                }

                type_key(&fixture, Key::a);
                assert_eq!(focused_name(&browser), "a.txt", "{mode:?}: a new prefix");
                assert!(!fixture.preview.is_open(), "{mode:?}");
                fixture.press(Key::space, NONE);
                wait_until(|| fixture.preview.is_open());
                assert!(
                    fixture.view.jump_to_name_active(),
                    "{mode:?}: Space does not end jump mode"
                );
                assert_eq!(fixture.view.jump_to_name_prefix(), "a", "{mode:?}");
                fixture.press(Key::space, NONE);
                wait_until(|| !fixture.preview.is_open());
                assert!(fixture.view.jump_to_name_active(), "{mode:?}");
                type_key(&fixture, Key::v);
                assert_eq!(focused_name(&browser), "avocado.txt", "{mode:?}");

                type_key(&fixture, Key::BackSpace);
                type_key(&fixture, Key::BackSpace);
                assert!(fixture.view.jump_to_name_active(), "{mode:?}");
                assert_eq!(
                    focused_name(&browser),
                    "a.txt",
                    "{mode:?}: an empty prefix keeps the selection"
                );
                type_key(&fixture, Key::BackSpace);
                assert!(!fixture.view.jump_to_name_active(), "{mode:?}");
                assert!(
                    !location_ends_with(browser.active_location(), "docs"),
                    "{mode:?}: Backspace does not navigate"
                );

                type_key(&fixture, Key::d);
                assert_eq!(focused_name(&browser), "docs", "{mode:?}");
                fixture.press(Key::Return, NONE);
                wait_until(|| location_ends_with(browser.active_location(), "docs"));
                assert!(
                    !fixture.view.jump_to_name_active(),
                    "{mode:?}: Enter ends it"
                );
                browser.navigate(Location::local(fixture._directory.path()));
                wait_until(|| {
                    !location_ends_with(browser.active_location(), "docs")
                        && browser
                            .column_snapshot(0)
                            .is_some_and(|column| !column.loading)
                });
                wait_until(|| entry_count(&browser) == 10);
            }
        },
    );
}

#[test]
fn jump_to_name_keeps_the_keys_while_previews_and_peeking_react() {
    crate::test_support::gtk_test(
        "ui::window::tests::keyboard_dispatch::jump_to_name::jump_to_name_keeps_the_keys_while_previews_and_peeking_react",
        || {
            let fixture = jump_fixture(TypingMode::JumpToName);
            let preferences = PreferenceManager::shared();
            preferences.set_columns_mirror_selection(true);
            preferences.set_folder_peeking(true);
            preferences.set_single_click_previews(true);
            pump(100);
            let browser = fixture.view.browser();
            for mode in [BrowserMode::Columns, BrowserMode::List, BrowserMode::Icons] {
                show_mode(&fixture, mode, "c.txt");

                type_key(&fixture, Key::d);
                assert_eq!(focused_name(&browser), "docs", "{mode:?}");
                pump(700);
                assert!(
                    fixture.view.jump_to_name_active(),
                    "{mode:?}: folder reaction"
                );
                assert!(fixture.view.item_view_has_focus(), "{mode:?}");
                assert!(fixture.press(Key::Escape, NONE), "{mode:?}");

                type_key(&fixture, Key::a);
                assert_eq!(focused_name(&browser), "a.txt", "{mode:?}");
                pump(700);
                assert!(fixture.view.jump_to_name_active(), "{mode:?}: file preview");
                assert!(fixture.view.item_view_has_focus(), "{mode:?}");
                type_key(&fixture, Key::p);
                assert_eq!(focused_name(&browser), "apple.txt", "{mode:?}");
                assert!(fixture.press(Key::Escape, NONE), "{mode:?}");
            }
        },
    );
}

#[test]
fn vim_keys_mode_starts_jump_mode_only_with_slash() {
    crate::test_support::gtk_test(
        "ui::window::tests::keyboard_dispatch::jump_to_name::vim_keys_mode_starts_jump_mode_only_with_slash",
        || {
            let fixture = jump_fixture(TypingMode::VimKeys);
            let browser = fixture.view.browser();
            for mode in [BrowserMode::Columns, BrowserMode::List, BrowserMode::Icons] {
                show_mode(&fixture, mode, "a.txt");
                assert!(!fixture.press(Key::b, NONE), "{mode:?}: letters are inert");
                assert!(!fixture.view.jump_to_name_active(), "{mode:?}");
                assert!(fixture.press(Key::j, NONE), "{mode:?}: j is still an arrow");

                select_named(&fixture, "apple.txt");
                type_key(&fixture, Key::slash);
                assert!(fixture.view.jump_to_name_active(), "{mode:?}");
                assert_eq!(fixture.view.jump_to_name_prefix(), "", "{mode:?}");
                assert_eq!(focused_name(&browser), "apple.txt", "{mode:?}");
                type_key(&fixture, Key::b);
                assert_eq!(focused_name(&browser), "b.txt", "{mode:?}");
                type_key(&fixture, Key::a);
                assert_eq!(focused_name(&browser), "banana.txt", "{mode:?}");
                type_key(&fixture, Key::Escape);
                assert!(!fixture.view.jump_to_name_active(), "{mode:?}");
                assert_eq!(focused_name(&browser), "banana.txt", "{mode:?}");

                assert!(fixture.press(Key::j, NONE), "{mode:?}: keys return");
            }
        },
    );
}

#[test]
fn jump_to_name_times_out_unless_started_with_slash() {
    crate::test_support::gtk_test(
        "ui::window::tests::keyboard_dispatch::jump_to_name::jump_to_name_times_out_unless_started_with_slash",
        || {
            let fixture = jump_fixture(TypingMode::JumpToName);
            let browser = fixture.view.browser();
            fixture
                .view
                .set_jump_to_name_idle_timeout(Duration::from_millis(150));
            select_named(&fixture, "c.txt");

            type_key(&fixture, Key::a);
            type_key(&fixture, Key::p);
            assert_eq!(focused_name(&browser), "apple.txt");
            wait_until(|| !fixture.view.jump_to_name_active());
            assert_eq!(fixture.view.jump_to_name_prefix(), "");
            assert_eq!(focused_name(&browser), "apple.txt", "timeout keeps it");
            type_key(&fixture, Key::b);
            assert_eq!(fixture.view.jump_to_name_prefix(), "b", "a fresh prefix");
            wait_until(|| !fixture.view.jump_to_name_active());

            type_key(&fixture, Key::slash);
            type_key(&fixture, Key::a);
            pump(600);
            assert!(fixture.view.jump_to_name_active(), "/ has no timeout");
            assert_eq!(fixture.view.jump_to_name_prefix(), "a");
            type_key(&fixture, Key::Escape);
            assert!(!fixture.view.jump_to_name_active());
        },
    );
}

#[test]
fn jump_to_name_yields_to_type_to_search_and_tenxer_mode() {
    crate::test_support::gtk_test(
        "ui::window::tests::keyboard_dispatch::jump_to_name::jump_to_name_yields_to_type_to_search_and_tenxer_mode",
        || {
            let fixture = jump_fixture(TypingMode::JumpToName);
            let preferences = PreferenceManager::shared();

            select_named(&fixture, "c.txt");
            type_key(&fixture, Key::a);
            assert!(fixture.view.jump_to_name_active());
            preferences.set_type_to_search(true);
            assert!(
                !fixture.view.jump_to_name_active(),
                "turning Type to search on ends the session"
            );
            select_named(&fixture, "c.txt");
            assert!(fixture.press(Key::a, NONE));
            assert!(fixture.view.filter_has_focus());
            assert!(!fixture.view.jump_to_name_active());
            assert!(fixture.press(Key::Escape, NONE));

            preferences.set_type_to_search(false);
            let preferences = footer_prompt::enable_tenxer(&fixture);
            select_named(&fixture, "c.txt");
            assert!(fixture.press(Key::slash, NONE));
            assert_eq!(
                fixture.shortcuts.open_prompt_kind(),
                Some(crate::ui::tenxer_mode::Prompt::Find)
            );
            assert!(!fixture.view.jump_to_name_active(), "10xer mode owns /");
            assert!(preferences.tenxer_mode());
        },
    );
}
