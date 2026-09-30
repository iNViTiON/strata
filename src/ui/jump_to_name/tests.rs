// SPDX-License-Identifier: MIT

use gtk::gdk::{Key, ModifierType};
use unicode_normalization::UnicodeNormalization;

use super::{Action, TypingMode, classify, cycle_index, prefix_matches};
use crate::services::fold_for_search;

const NONE: ModifierType = ModifierType::empty();
const CONTROL: ModifierType = ModifierType::CONTROL_MASK;
const SHIFT: ModifierType = ModifierType::SHIFT_MASK;

#[test]
fn inactive_keys_start_jump_mode_only_where_the_typing_mode_allows() {
    use Action::{Begin, Ignore};
    for (mode, key, modifiers, expected) in [
        (TypingMode::VimKeys, Key::slash, NONE, Begin(None)),
        (TypingMode::JumpToName, Key::slash, NONE, Begin(None)),
        (TypingMode::VimKeys, Key::a, NONE, Ignore),
        (TypingMode::VimKeys, Key::j, NONE, Ignore),
        (TypingMode::VimKeys, Key::y, NONE, Ignore),
        (TypingMode::JumpToName, Key::a, NONE, Begin(Some('a'))),
        (TypingMode::JumpToName, Key::j, NONE, Begin(Some('j'))),
        (TypingMode::JumpToName, Key::p, NONE, Begin(Some('p'))),
        (TypingMode::JumpToName, Key::A, SHIFT, Begin(Some('A'))),
        (TypingMode::JumpToName, Key::_1, NONE, Begin(Some('1'))),
        (TypingMode::JumpToName, Key::period, NONE, Begin(Some('.'))),
        (TypingMode::JumpToName, Key::eacute, NONE, Begin(Some('é'))),
        (TypingMode::JumpToName, Key::space, NONE, Ignore),
        (TypingMode::JumpToName, Key::space, SHIFT, Ignore),
        (TypingMode::JumpToName, Key::a, CONTROL, Ignore),
        (
            TypingMode::JumpToName,
            Key::a,
            ModifierType::ALT_MASK,
            Ignore,
        ),
        (
            TypingMode::JumpToName,
            Key::a,
            ModifierType::SUPER_MASK,
            Ignore,
        ),
        (TypingMode::JumpToName, Key::slash, CONTROL, Ignore),
        (TypingMode::JumpToName, Key::n, CONTROL, Ignore),
        (TypingMode::JumpToName, Key::F5, NONE, Ignore),
        (TypingMode::JumpToName, Key::Escape, NONE, Ignore),
        (TypingMode::JumpToName, Key::BackSpace, NONE, Ignore),
        (TypingMode::JumpToName, Key::Return, NONE, Ignore),
        (TypingMode::JumpToName, Key::Left, NONE, Ignore),
        (TypingMode::JumpToName, Key::Shift_L, NONE, Ignore),
    ] {
        assert_eq!(
            classify(key, modifiers, false, mode),
            expected,
            "{mode:?} {key:?} {modifiers:?}"
        );
    }
}

#[test]
fn active_jump_mode_claims_typing_and_releases_everything_else() {
    use Action::{Backspace, Exit, Keep, Release, Step, Type};
    for mode in [TypingMode::VimKeys, TypingMode::JumpToName] {
        for (key, modifiers, expected) in [
            (Key::a, NONE, Type('a')),
            (Key::h, NONE, Type('h')),
            (Key::j, NONE, Type('j')),
            (Key::y, NONE, Type('y')),
            (Key::A, SHIFT, Type('A')),
            (Key::period, NONE, Type('.')),
            (Key::slash, NONE, Type('/')),
            (Key::n, NONE, Type('n')),
            (Key::n, CONTROL, Step(1)),
            (Key::N, CONTROL, Step(1)),
            (Key::p, CONTROL, Step(-1)),
            (Key::p, CONTROL | ModifierType::LOCK_MASK, Step(-1)),
            (Key::n, CONTROL | SHIFT, Release),
            (Key::n, ModifierType::ALT_MASK, Release),
            (Key::a, CONTROL, Release),
            (Key::BackSpace, NONE, Backspace),
            (Key::BackSpace, SHIFT, Backspace),
            (Key::BackSpace, CONTROL, Release),
            (Key::Escape, NONE, Exit),
            (Key::Escape, SHIFT, Exit),
            (Key::Escape, CONTROL, Release),
            (Key::Return, NONE, Release),
            (Key::KP_Enter, NONE, Release),
            (Key::Return, ModifierType::ALT_MASK, Release),
            (Key::Left, NONE, Release),
            (Key::Down, NONE, Release),
            (Key::Home, NONE, Release),
            (Key::Page_Down, NONE, Release),
            (Key::Tab, NONE, Release),
            (Key::Delete, NONE, Release),
            (Key::F2, NONE, Release),
            (Key::space, NONE, Keep),
            (Key::space, SHIFT, Keep),
            (Key::space, CONTROL, Release),
            (Key::Shift_L, SHIFT, Keep),
            (Key::Control_R, CONTROL, Keep),
            (Key::Caps_Lock, NONE, Keep),
            (Key::ISO_Level3_Shift, NONE, Keep),
        ] {
            assert_eq!(
                classify(key, modifiers, true, mode),
                expected,
                "{mode:?} {key:?} {modifiers:?}"
            );
        }
    }
}

#[test]
fn prefixes_match_names_case_insensitively_from_the_start() {
    for (name, prefix, expected) in [
        ("Report.txt", "rep", true),
        ("report.txt", "REP", true),
        ("Report.txt", "port", false),
        ("Report.txt", "", true),
        ("Report.txt", "report.txt", true),
        ("Report.txt", "report.txt.bak", false),
        (".bashrc", "b", false),
        (".bashrc", ".b", true),
        ("bin", "b", true),
        ("Ärger.md", "är", true),
        ("Ärger.md", "ÄR", true),
        ("Ärger.md", "ar", false),
        ("ÅNGSTRÖM", "ång", true),
        ("日本語.txt", "日本", true),
    ] {
        assert_eq!(
            prefix_matches(name, &fold_for_search(prefix)),
            expected,
            "{prefix:?} against {name:?}"
        );
    }
}

#[test]
fn prefixes_match_across_unicode_normalization_forms() {
    let composed = "Ärger.md";
    let decomposed: String = composed.nfd().collect();
    assert_ne!(composed, decomposed);
    let composed_prefix: String = "är".into();
    let decomposed_prefix: String = composed_prefix.nfd().collect();
    for (name, prefix, expected) in [
        (composed, &composed_prefix, true),
        (composed, &decomposed_prefix, true),
        (decomposed.as_str(), &composed_prefix, true),
        (decomposed.as_str(), &decomposed_prefix, true),
        (decomposed.as_str(), &"ar".to_owned(), false),
    ] {
        assert_eq!(
            prefix_matches(name, &fold_for_search(prefix)),
            expected,
            "{prefix:?} against {name:?}"
        );
    }
}

#[test]
fn cycling_wraps_in_both_directions_and_starts_at_the_ends_without_a_cursor() {
    for (len, current, backward, offsets, expected) in [
        (4, Some(1), false, vec![1, 2, 3, 4], vec![2, 3, 0, 1]),
        (4, Some(1), true, vec![1, 2, 3, 4], vec![0, 3, 2, 1]),
        (4, Some(3), false, vec![1], vec![0]),
        (4, Some(0), true, vec![1], vec![3]),
        (4, None, false, vec![1, 2], vec![0, 1]),
        (4, None, true, vec![1, 2], vec![3, 2]),
        (1, Some(0), false, vec![1], vec![0]),
        (1, Some(0), true, vec![1], vec![0]),
    ] {
        let walked: Vec<_> = offsets
            .into_iter()
            .map(|offset| cycle_index(len, current, backward, offset))
            .collect();
        assert_eq!(walked, expected, "{len} {current:?} backward={backward}");
    }
}
