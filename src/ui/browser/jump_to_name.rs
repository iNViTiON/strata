// SPDX-License-Identifier: MIT

//! Type-ahead jump mode: a typed prefix selects the first entry of the focused
//! pane whose name starts with it. Key decoding lives in `ui::jump_to_name`; this
//! state only takes committed text, so an input method can feed it later.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

use gtk::{glib, prelude::*};

use super::{BrowserView, ViewState};
use crate::{
    services::fold_for_search,
    ui::{
        jump_to_name::{cycle_index, prefix_matches},
        preferences::PreferenceManager,
    },
};

const IDLE_TIMEOUT: Duration = Duration::from_secs(1);
const INDICATOR_MARGIN: i32 = 10;
const PLACEHOLDER: &str = "Type a name";

pub(super) struct JumpState {
    prefix: RefCell<String>,
    active: Cell<bool>,
    /// Entered with `/`: only Esc or a navigation key ends it.
    sticky: Cell<bool>,
    idle_timeout: Cell<Duration>,
    idle: RefCell<Option<glib::SourceId>>,
    revealer: gtk::Revealer,
    pill: gtk::Box,
    label: gtk::Label,
}

impl JumpState {
    pub(super) fn new() -> Self {
        let label = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(24)
            .build();
        label.add_css_class("jump-to-name-text");
        let pill = gtk::Box::new(gtk::Orientation::Horizontal, 7);
        pill.add_css_class("jump-to-name");
        pill.append(&crate::assets::chrome_icon(crate::assets::icons::SEARCH));
        pill.append(&label);
        let revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::Crossfade)
            .transition_duration(100)
            .halign(gtk::Align::End)
            .valign(gtk::Align::End)
            .child(&pill)
            .build();
        for widget in [revealer.upcast_ref::<gtk::Widget>(), pill.upcast_ref()] {
            widget.set_can_target(false);
            widget.set_can_focus(false);
        }
        Self {
            prefix: RefCell::default(),
            active: Cell::new(false),
            sticky: Cell::new(false),
            idle_timeout: Cell::new(IDLE_TIMEOUT),
            idle: RefCell::new(None),
            revealer,
            pill,
            label,
        }
    }

    fn install(&self, overlay: &gtk::Overlay) {
        overlay.add_overlay(&self.revealer);
    }

    fn show(&self, overlay: &gtk::Overlay, matched: bool) {
        let prefix = self.prefix.borrow();
        let empty = prefix.is_empty();
        self.label
            .set_text(if empty { PLACEHOLDER } else { &prefix });
        self.label.set_css_classes(&["jump-to-name-text"]);
        if empty {
            self.label.add_css_class("jump-to-name-placeholder");
        }
        if matched {
            self.pill.remove_css_class("no-match");
        } else {
            self.pill.add_css_class("no-match");
        }
        self.place(overlay);
        self.revealer.set_reveal_child(true);
    }

    /// Anchors the pill to the bottom-right of the focused pane.
    fn place(&self, overlay: &gtk::Overlay) {
        let (right, bottom) = overlay
            .root()
            .and_then(|root| root.focus())
            .and_then(|focused| crate::ui::scrolling::focused_collection(&focused))
            .and_then(|(_, scroll)| scroll.compute_bounds(overlay))
            .map_or((0.0, 0.0), |bounds| {
                (
                    overlay.width() as f32 - bounds.x() - bounds.width(),
                    overlay.height() as f32 - bounds.y() - bounds.height(),
                )
            });
        self.revealer
            .set_margin_end(INDICATOR_MARGIN + right.max(0.0) as i32);
        self.revealer
            .set_margin_bottom(INDICATOR_MARGIN + bottom.max(0.0) as i32);
    }
}

/// Jump mode also ends on a click, when focus leaves the view, and when the
/// preferences stop enabling it.
pub(super) fn bind(state: &Rc<ViewState>, preferences: &PreferenceManager) {
    state.jump.install(&state.overlay);
    let click = gtk::GestureClick::new();
    click.set_button(0);
    click.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak_state = Rc::downgrade(state);
    click.connect_pressed(move |_, _, _, _| {
        if let Some(state) = weak_state.upgrade() {
            state.end_jump_to_name();
        }
    });
    state.overlay.add_controller(click);
    let focus = gtk::EventControllerFocus::new();
    let weak_state = Rc::downgrade(state);
    focus.connect_leave(move |_| {
        if let Some(state) = weak_state.upgrade() {
            state.end_jump_to_name();
        }
    });
    state.overlay.add_controller(focus);
    let weak_state = Rc::downgrade(state);
    preferences.bind_preference(
        &state.overlay,
        crate::ui::jump_to_name::available,
        move |_, available| {
            if !available && let Some(state) = weak_state.upgrade() {
                state.end_jump_to_name();
            }
        },
    );
}

impl ViewState {
    pub(super) fn end_jump_to_name(&self) {
        let jump = &self.jump;
        if !jump.active.replace(false) {
            return;
        }
        jump.sticky.set(false);
        jump.prefix.borrow_mut().clear();
        if let Some(source) = jump.idle.borrow_mut().take() {
            source.remove();
        }
        jump.revealer.set_reveal_child(false);
        jump.pill.remove_css_class("no-match");
    }
}

impl BrowserView {
    pub(in crate::ui) fn jump_to_name_active(&self) -> bool {
        self.state.jump.active.get()
    }

    /// Recursive results have their own rows and cursor.
    pub(in crate::ui) fn jump_to_name_supported(&self) -> bool {
        !self
            .filter_target()
            .is_some_and(|target| target.results_view().is_some())
    }

    /// Starts with an empty prefix. `sticky` sessions ignore the idle timeout.
    pub(in crate::ui) fn begin_jump_to_name(&self, sticky: bool) {
        let jump = &self.state.jump;
        jump.prefix.borrow_mut().clear();
        jump.active.set(true);
        jump.sticky.set(sticky);
        jump.show(&self.state.overlay, true);
        self.touch_jump_to_name();
    }

    /// Restarts the idle countdown of a timed session.
    pub(in crate::ui) fn touch_jump_to_name(&self) {
        let jump = &self.state.jump;
        if let Some(source) = jump.idle.borrow_mut().take() {
            source.remove();
        }
        if !jump.active.get() || jump.sticky.get() {
            return;
        }
        let weak_state = Rc::downgrade(&self.state);
        let source = glib::timeout_add_local_once(jump.idle_timeout.get(), move || {
            if let Some(state) = weak_state.upgrade() {
                state.jump.idle.borrow_mut().take();
                state.end_jump_to_name();
            }
        });
        jump.idle.replace(Some(source));
    }

    pub(in crate::ui) fn end_jump_to_name(&self) {
        self.state.end_jump_to_name();
    }

    /// Appends committed text and selects the first match. Typing the letter
    /// that is the whole prefix again cycles to the next match instead.
    pub(in crate::ui) fn jump_to_name_type(&self, text: &str) {
        let repeated = {
            let prefix = self.state.jump.prefix.borrow();
            !prefix.is_empty() && fold_for_search(&prefix) == fold_for_search(text)
        };
        if repeated && text.chars().count() == 1 {
            self.jump_to_name_step(1);
            return;
        }
        self.state.jump.prefix.borrow_mut().push_str(text);
        self.jump_to_prefix();
    }

    /// Drops the last character; an empty prefix leaves jump mode.
    pub(in crate::ui) fn jump_to_name_backspace(&self) {
        if self.state.jump.prefix.borrow_mut().pop().is_none() {
            self.end_jump_to_name();
        } else if self.state.jump.prefix.borrow().is_empty() {
            self.state.jump.show(&self.state.overlay, true);
            self.touch_jump_to_name();
        } else {
            self.jump_to_prefix();
        }
    }

    /// Moves to the next (`1`) or previous (`-1`) match, wrapping.
    pub(in crate::ui) fn jump_to_name_step(&self, direction: i32) {
        let matched = self.select_matching(Some(direction));
        self.state.jump.show(&self.state.overlay, matched);
        self.touch_jump_to_name();
    }

    #[cfg(test)]
    pub(in crate::ui) fn jump_to_name_prefix(&self) -> String {
        self.state.jump.prefix.borrow().clone()
    }

    #[cfg(test)]
    pub(in crate::ui) fn jump_to_name_matched(&self) -> bool {
        !self.state.jump.pill.has_css_class("no-match")
    }

    #[cfg(test)]
    pub(in crate::ui) fn set_jump_to_name_idle_timeout(&self, timeout: Duration) {
        self.state.jump.idle_timeout.set(timeout);
    }

    fn jump_to_prefix(&self) {
        let matched = self.select_matching(None);
        self.state.jump.show(&self.state.overlay, matched);
        self.touch_jump_to_name();
    }

    /// Selects the first match from the top, or the next match from the cursor
    /// when `step` is given. A miss leaves the selection alone.
    fn select_matching(&self, step: Option<i32>) -> bool {
        let Some(depth) = self.focused_listing_depth() else {
            return false;
        };
        let Some(order) = self.displayed_order(depth) else {
            return false;
        };
        let prefix = fold_for_search(&self.state.jump.prefix.borrow());
        let matches = |position: usize| {
            self.state
                .browser
                .entry_at(depth, position)
                .is_some_and(|entry| prefix_matches(&entry.display_name, &prefix))
        };
        let found = match step {
            None => order.iter().copied().find(|&position| matches(position)),
            Some(direction) => {
                let current = self.cursor_index(depth, &order);
                (1..=order.len())
                    .map(|offset| order[cycle_index(order.len(), current, direction < 0, offset)])
                    .find(|&position| matches(position))
            }
        };
        let Some(position) = found else {
            return false;
        };
        self.keyboard_navigation();
        self.state.browser.select(depth, position);
        self.scroll_cursor_into_view(depth, position, true);
        true
    }
}
