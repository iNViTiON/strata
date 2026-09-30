// SPDX-License-Identifier: MIT

//! Shift+Space opens the expanded preview. While it is up, this stage owns
//! the keys: arrows either drive the preview or change file, depending on
//! whether Shift is held and the "Hold Shift to control the preview" setting.

use std::rc::Rc;

use gtk::{
    gdk::{Key, ModifierType as Modifiers},
    glib::Propagation,
};

use super::{Dispatcher, KeyResult, command_modifiers, preview::passes_through_preview};
use crate::{
    app::Browser,
    ui::preview::{DocumentScroll, PreviewArrow, PreviewSurface, ZoomStep, preview_target},
};

impl Dispatcher {
    pub(super) fn expanded_preview(
        &self,
        browser: &Rc<Browser>,
        key: Key,
        modifiers: Modifiers,
    ) -> KeyResult {
        let mods = command_modifiers(modifiers);
        if self.preview.is_expanded() {
            return self.expanded_key(key, mods);
        }
        (key == Key::space && mods == Modifiers::SHIFT_MASK)
            .then(|| self.expand_from_listing(browser))
            .flatten()
    }

    /// Only from the file list or the preview itself: text fields, the
    /// sidebar and header buttons keep Shift+Space for themselves.
    fn expand_from_listing(&self, browser: &Rc<Browser>) -> KeyResult {
        let focused = gtk::prelude::RootExt::focus(&self.window)?;
        let in_listing = if self.preview.owns_focus(Some(&focused)) {
            self.preview.surface(&focused) != PreviewSurface::Text
        } else {
            self.view.item_view_has_focus()
        };
        if !in_listing {
            return None;
        }
        let entry = if self.view.selected_search_results().is_some() {
            self.view.selected_search_result()
        } else {
            browser.focused_entry()
        };
        let target = preview_target(entry).map(|entry| (entry, browser.active_depth()));
        if target.is_none() && !self.preview.is_enabled() {
            self.shortcuts.show_feedback("Nothing to preview");
            return Some(Propagation::Stop);
        }
        self.preview.expand(target);
        Some(Propagation::Stop)
    }

    /// Keys typed in the fullscreen window, which has no dispatcher of its own.
    pub(super) fn expanded_window_key(&self, key: Key, modifiers: Modifiers) -> Propagation {
        self.expanded_key(key, command_modifiers(modifiers))
            .unwrap_or(Propagation::Proceed)
    }

    fn expanded_key(&self, key: Key, mods: Modifiers) -> KeyResult {
        if key == Key::Escape && mods.is_empty() {
            self.preview.collapse();
            return Some(Propagation::Stop);
        }
        if self.preview.is_collapsing() {
            return Some(Propagation::Stop);
        }
        let typing = self.preview.focused_widget().is_some_and(|focused| {
            self.preview.owns_focus(Some(&focused))
                && self.preview.surface(&focused) == PreviewSurface::Text
        });
        if typing {
            return Some(Propagation::Proceed);
        }
        if passes_through_preview(key, mods) {
            return None;
        }
        if matches!(key, Key::Tab | Key::ISO_Left_Tab) {
            self.preview.move_focus_within(key == Key::Tab);
            return Some(Propagation::Stop);
        }
        if key == Key::space {
            if mods == Modifiers::SHIFT_MASK {
                self.preview.collapse();
            } else if mods.is_empty() && !self.preview.expanded_media_key(Key::space) {
                self.preview.close_expanded();
            }
            return Some(Propagation::Stop);
        }
        if let Some(arrow) = arrow(key) {
            self.expanded_arrow(arrow, mods);
            return Some(Propagation::Stop);
        }
        if let Some(step) = zoom_step(key)
            && (mods.is_empty() || mods == Modifiers::SHIFT_MASK)
        {
            self.preview.zoom(step);
        } else if mods.is_empty() {
            self.expanded_plain_key(key);
        }
        Some(Propagation::Stop)
    }

    fn expanded_arrow(&self, arrow: PreviewArrow, mods: Modifiers) {
        if mods != Modifiers::SHIFT_MASK && !mods.is_empty() {
            return;
        }
        let shift_controls = self
            .type_to_search
            .preferences
            .expanded_preview_shift_controls();
        let controls_preview = (mods == Modifiers::SHIFT_MASK) == shift_controls;
        if controls_preview && self.preview.control(arrow) {
            return;
        }
        let direction = match arrow {
            PreviewArrow::Up | PreviewArrow::Left => -1,
            PreviewArrow::Down | PreviewArrow::Right => 1,
        };
        self.view.place_previewable_cursor(direction);
    }

    fn expanded_plain_key(&self, key: Key) {
        let motion = match key {
            Key::Page_Up | Key::KP_Page_Up => DocumentScroll::Page(-1),
            Key::Page_Down | Key::KP_Page_Down => DocumentScroll::Page(1),
            Key::Home | Key::KP_Home => DocumentScroll::Start,
            Key::End | Key::KP_End => DocumentScroll::End,
            Key::m | Key::M => {
                self.preview.expanded_media_key(Key::m);
                return;
            }
            _ => return,
        };
        self.preview.scroll_document(motion);
    }
}

fn zoom_step(key: Key) -> Option<ZoomStep> {
    match key {
        Key::plus | Key::equal | Key::KP_Add => Some(ZoomStep::In),
        Key::minus | Key::underscore | Key::KP_Subtract => Some(ZoomStep::Out),
        Key::_0 | Key::KP_0 => Some(ZoomStep::Fit),
        _ => None,
    }
}

fn arrow(key: Key) -> Option<PreviewArrow> {
    match key {
        Key::Up | Key::KP_Up => Some(PreviewArrow::Up),
        Key::Down | Key::KP_Down => Some(PreviewArrow::Down),
        Key::Left | Key::KP_Left => Some(PreviewArrow::Left),
        Key::Right | Key::KP_Right => Some(PreviewArrow::Right),
        _ => None,
    }
}
