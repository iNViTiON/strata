// SPDX-License-Identifier: MIT

//! Shift+Space opens the expanded preview. While it is up, this stage owns
//! the keys: arrows either drive the preview or change file, depending on
//! whether Shift is held and the "Hold Shift to control the preview" setting.

use std::rc::Rc;

use gtk::{gdk::ModifierType as Modifiers, glib::Propagation};

use super::{Dispatcher, KeyResult, preview::passes_through_preview};
use crate::{
    app::Browser,
    ui::{
        keys::{KeyPress, SHORTCUT_MODIFIERS, bindings::*},
        preview::{
            DocumentScroll, MediaCommand, PreviewArrow, PreviewSurface, ZoomStep, preview_target,
        },
    },
};

impl Dispatcher {
    pub(super) fn expanded_preview(&self, browser: &Rc<Browser>, press: &KeyPress) -> KeyResult {
        if self.preview.is_expanded() {
            return self.expanded_key(press);
        }
        press
            .is(EXPAND_PREVIEW)
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
    pub(super) fn expanded_window_key(&self, press: &KeyPress) -> Propagation {
        self.expanded_key(press).unwrap_or(Propagation::Proceed)
    }

    fn expanded_key(&self, press: &KeyPress) -> KeyResult {
        if press.is(ESCAPE) {
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
        if passes_through_preview(press) {
            return None;
        }
        if press.is_any(&[FOCUS_NEXT, FOCUS_PREVIOUS]) {
            self.preview.move_focus_within(press.is(FOCUS_NEXT));
            return Some(Propagation::Stop);
        }
        if press.is(SPACE.ignoring(SHORTCUT_MODIFIERS)) {
            if press.is(EXPAND_PREVIEW) {
                self.preview.collapse();
            } else if press.is(SPACE) && !self.preview.expanded_media_key(MediaCommand::PlayPause) {
                self.preview.close_expanded();
            }
            return Some(Propagation::Stop);
        }
        if let Some(arrow) = arrow(press) {
            self.expanded_arrow(arrow, press);
            return Some(Propagation::Stop);
        }
        if let Some(step) = zoom_step(press) {
            self.preview.zoom(step);
        } else {
            self.expanded_plain_key(press);
        }
        Some(Propagation::Stop)
    }

    fn expanded_arrow(&self, arrow: PreviewArrow, press: &KeyPress) {
        let mods = press.modifiers();
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

    fn expanded_plain_key(&self, press: &KeyPress) {
        let motion = if press.is(PAGE_UP) {
            DocumentScroll::Page(-1)
        } else if press.is(PAGE_DOWN) {
            DocumentScroll::Page(1)
        } else if press.is(FIRST_ITEM) {
            DocumentScroll::Start
        } else if press.is(LAST_ITEM) {
            DocumentScroll::End
        } else {
            if press.is(EXPANDED_MUTE) {
                self.preview.expanded_media_key(MediaCommand::Mute);
            }
            return;
        };
        self.preview.scroll_document(motion);
    }
}

fn zoom_step(press: &KeyPress) -> Option<ZoomStep> {
    if press.is_any(&[EXPANDED_ZOOM_IN, EXPANDED_ZOOM_IN_EQUAL]) {
        Some(ZoomStep::In)
    } else if press.is(EXPANDED_ZOOM_OUT) {
        Some(ZoomStep::Out)
    } else if press.is(EXPANDED_ZOOM_FIT) {
        Some(ZoomStep::Fit)
    } else {
        None
    }
}

fn arrow(press: &KeyPress) -> Option<PreviewArrow> {
    [
        (UP, PreviewArrow::Up),
        (DOWN, PreviewArrow::Down),
        (LEFT, PreviewArrow::Left),
        (RIGHT, PreviewArrow::Right),
    ]
    .into_iter()
    .find(|(binding, _)| press.is(binding.ignoring(SHORTCUT_MODIFIERS)))
    .map(|(_, arrow)| arrow)
}
