// SPDX-License-Identifier: MIT

//! Type-ahead jump to a file name while **Type to search** is off. The window and
//! the portal chooser share this key routing; the prefix state, matching and
//! indicator live on `BrowserView`, which takes committed text rather than keys.

use gtk::{gdk::ModifierType, glib::Propagation};

use super::{
    browser::BrowserView,
    keys::{
        KeyPress,
        bindings::{BACKSPACE, ESCAPE_ANY_SHIFT, NEXT_NAME, PREVIOUS_NAME},
    },
    preferences::{PreferenceManager, TypingMode},
};
use crate::services::fold_for_search;

#[cfg(test)]
mod tests;

#[derive(Debug, Eq, PartialEq)]
enum Action {
    /// `None` for `/`, which starts a session without the idle timeout.
    Begin(Option<char>),
    Type(char),
    Backspace,
    Step(i32),
    /// Leaves jump mode and consumes the key.
    Exit,
    /// Leaves jump mode and lets the key do its normal job.
    Release,
    /// Stays in jump mode and lets the key do its normal job.
    Keep,
    Ignore,
}

pub(in crate::ui) fn available(preferences: &PreferenceManager) -> bool {
    !preferences.type_to_search() && !preferences.tenxer_mode()
}

pub(in crate::ui) fn prefix_matches(name: &str, folded_prefix: &str) -> bool {
    fold_for_search(name).starts_with(folded_prefix)
}

/// The `offset`-th candidate (1-based) after `current`, wrapping. Without a
/// current item a forward walk starts at the top and a backward one at the end.
pub(in crate::ui) fn cycle_index(
    len: usize,
    current: Option<usize>,
    backward: bool,
    offset: usize,
) -> usize {
    match (current, backward) {
        (Some(index), false) => (index + offset) % len,
        (Some(index), true) => (index + len - offset % len) % len,
        (None, false) => offset - 1,
        (None, true) => len - offset,
    }
}

pub(in crate::ui) fn handle_key(
    view: &BrowserView,
    preferences: &PreferenceManager,
    press: &KeyPress,
) -> Option<Propagation> {
    let active = view.jump_to_name_active();
    if !available(preferences)
        || !view.item_view_has_focus()
        || view.rename_is_active()
        || view.new_entry_is_active()
        || !view.jump_to_name_supported()
    {
        if active {
            view.end_jump_to_name();
        }
        return None;
    }
    match classify(press, active, preferences.typing_mode()) {
        Action::Ignore => None,
        Action::Keep => {
            view.touch_jump_to_name();
            None
        }
        Action::Release => {
            view.end_jump_to_name();
            None
        }
        Action::Exit => {
            view.end_jump_to_name();
            Some(Propagation::Stop)
        }
        Action::Begin(text) => {
            view.begin_jump_to_name(text.is_none());
            if let Some(character) = text {
                view.jump_to_name_type(&character.to_string());
            }
            Some(Propagation::Stop)
        }
        Action::Type(character) => {
            view.jump_to_name_type(&character.to_string());
            Some(Propagation::Stop)
        }
        Action::Backspace => {
            view.jump_to_name_backspace();
            Some(Propagation::Stop)
        }
        Action::Step(direction) => {
            view.jump_to_name_step(direction);
            Some(Propagation::Stop)
        }
    }
}

fn classify(press: &KeyPress, active: bool, mode: TypingMode) -> Action {
    let typed = press.text().filter(|character| *character != ' ');
    if !active {
        return match typed {
            Some('/') => Action::Begin(None),
            Some(character) if mode == TypingMode::JumpToName => Action::Begin(Some(character)),
            _ => Action::Ignore,
        };
    }
    if press.is(ESCAPE_ANY_SHIFT) {
        return Action::Exit;
    }
    if press.is_modifier() || press.text() == Some(' ') {
        return Action::Keep;
    }
    if press.is(NEXT_NAME) {
        Action::Step(1)
    } else if press.is(PREVIOUS_NAME) {
        Action::Step(-1)
    } else if press.is(BACKSPACE.ignoring(ModifierType::SHIFT_MASK)) {
        Action::Backspace
    } else {
        typed.map_or(Action::Release, Action::Type)
    }
}
