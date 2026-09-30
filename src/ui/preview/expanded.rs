// SPDX-License-Identifier: MIT

//! The expanded preview: the drawer's live pane moves into a large host and
//! back, so nothing is reloaded or restarted when its size changes.

use gtk::{
    gdk::{Key, ModifierType},
    glib::Propagation,
};

use super::*;
use crate::ui::preferences::ExpandedPreviewStyle;

mod layer;

use layer::ExpandedLayer;

type KeyHandler = Rc<dyn Fn(Key, ModifierType) -> Propagation>;

pub(super) const EXPANDED_CLASS: &str = "preview-expanded";
const TRANSITION_MS: f64 = 260.0;
const SETTLE_FRAMES: u32 = 3;
const MIN_FALLBACK_SIZE: i32 = 480;
const MAX_SETTLE_FRAMES: u32 = 180;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::ui) enum PreviewArrow {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::ui) enum ZoomStep {
    In,
    Out,
    Fit,
}

/// The middle of the scrolled document as fractions of its size, so it can be
/// found again after the content reflows to a new width.
#[derive(Clone, Copy)]
struct Anchor {
    horizontal: f64,
    vertical: f64,
}

#[derive(Default)]
pub(super) struct ExpandedState {
    this: RefCell<std::rc::Weak<PreviewState>>,
    available: Cell<bool>,
    layout_generation: Cell<u64>,
    anchor: Cell<Option<Anchor>>,
    relocating: Cell<bool>,
    collapsing: Cell<bool>,
    host: RefCell<Option<Host>>,
    key_handler: RefCell<Option<KeyHandler>>,
}

#[derive(Clone)]
enum Host {
    Overlay {
        layer: ExpandedLayer,
        overlay: gtk::Overlay,
    },
    Window {
        window: gtk::Window,
        main: glib::WeakRef<gtk::Window>,
        destroyed: Rc<RefCell<Option<glib::SignalHandlerId>>>,
    },
}

impl ExpandedState {
    pub(super) fn is_active(&self) -> bool {
        self.host.borrow().is_some()
    }

    pub(super) fn is_relocating(&self) -> bool {
        self.relocating.get()
    }
}

impl PreviewDrawer {
    /// Lets this drawer expand. Only the main window's drawer does; the file
    /// chooser keeps its small preview.
    pub(in crate::ui) fn enable_expansion(&self) {
        self.state.expanded.available.set(true);
        self.state.expand_button.set_visible(true);
    }

    /// Runs the expanded view's keys for hosts that have no window-level
    /// dispatcher of their own, such as the fullscreen window.
    pub(in crate::ui) fn set_expanded_key_handler(
        &self,
        handler: impl Fn(Key, ModifierType) -> Propagation + 'static,
    ) {
        self.state
            .expanded
            .key_handler
            .replace(Some(Rc::new(handler)));
    }

    /// Tab cycles through the expanded view's own controls and never reaches the
    /// window hidden behind it.
    pub(in crate::ui) fn move_focus_within(&self, forward: bool) {
        let direction = if forward {
            gtk::DirectionType::TabForward
        } else {
            gtk::DirectionType::TabBackward
        };
        let pane = &self.state.pane;
        if !pane.child_focus(direction) {
            pane.child_focus(direction);
        }
    }

    /// Where keyboard focus is, in whichever window currently holds the pane.
    pub(in crate::ui) fn focused_widget(&self) -> Option<gtk::Widget> {
        self.state.pane.root().and_then(|root| root.focus())
    }

    pub(in crate::ui) fn is_expanded(&self) -> bool {
        self.state.expanded.is_active()
    }

    pub(in crate::ui) fn is_collapsing(&self) -> bool {
        self.state.expanded.collapsing.get()
    }

    /// Expands the open preview, or opens `target` directly into the expanded
    /// view when the drawer is closed.
    pub(in crate::ui) fn expand(&self, target: Option<(FileEntry, Option<usize>)>) -> bool {
        self.state.expand(target)
    }

    pub(in crate::ui) fn collapse(&self) {
        self.state.collapse(None);
    }

    /// Leaves the expanded view, then closes the preview.
    pub(in crate::ui) fn close_expanded(&self) {
        let weak = Rc::downgrade(&self.state);
        self.state.collapse(Some(Box::new(move || {
            if let Some(state) = weak.upgrade() {
                state.close();
            }
        })));
    }

    /// Applies an arrow to whatever the preview shows. `false` means the
    /// content has nothing to move, so the arrow should change file instead.
    pub(in crate::ui) fn control(&self, arrow: PreviewArrow) -> bool {
        self.state.control(arrow)
    }

    /// Zooms an image or PDF. `false` when the content cannot zoom.
    pub(in crate::ui) fn zoom(&self, step: ZoomStep) -> bool {
        self.state.zoom(step)
    }

    /// Plain media keys (play/pause, mute) even when the drawer is hidden.
    pub(in crate::ui) fn expanded_media_key(&self, key: Key) -> bool {
        self.state.media_command(key)
    }
}

impl PreviewState {
    pub(super) fn install_expand_button(self: &Rc<Self>) {
        self.expanded.this.replace(Rc::downgrade(self));
        let weak = Rc::downgrade(self);
        self.expand_button.connect_clicked(move |_| {
            let Some(state) = weak.upgrade() else {
                return;
            };
            if state.expanded.is_active() {
                state.collapse(None);
            } else {
                state.expand(None);
            }
        });
    }

    fn expand(self: &Rc<Self>, target: Option<(FileEntry, Option<usize>)>) -> bool {
        if !self.expanded.available.get() || self.expanded.is_active() {
            return false;
        }
        let was_enabled = self.is_enabled();
        if !was_enabled && target.is_none() {
            return false;
        }
        let Some(main) = self.pane.root().and_downcast::<gtk::Window>() else {
            return false;
        };
        let style = super::super::preferences::PreferenceManager::shared().expanded_preview_style();
        let overlay = super::super::modal::window_overlay(&self.pane);
        if style == ExpandedPreviewStyle::Overlay && overlay.is_none() {
            return false;
        }
        let origin = overlay
            .as_ref()
            .and_then(|overlay| self.drawer_bounds(overlay));
        let placeholder = gtk::Box::new(gtk::Orientation::Vertical, 0);
        placeholder.add_css_class("preview-pane");
        self.expanded.anchor.set(self.capture_anchor());
        self.expanded.relocating.set(true);
        self.revealer.set_child(Some(&placeholder));
        self.pane.add_css_class(EXPANDED_CLASS);
        let host = match (style, overlay) {
            (ExpandedPreviewStyle::Overlay, Some(overlay)) => self.attach_overlay(overlay, origin),
            _ => self.attach_window(&main),
        };
        self.expanded.relocating.set(false);
        self.expanded.host.replace(Some(host.clone()));
        self.sync_expand_button();
        if let Some((entry, depth)) = target
            && !was_enabled
        {
            self.show(entry, depth);
        }
        self.take_keyboard();
        match host {
            Host::Overlay { layer, .. } => {
                self.settle_layout(false);
                let weak = Rc::downgrade(self);
                layer.animate_to(1.0, TRANSITION_MS, move || {
                    if let Some(state) = weak.upgrade() {
                        state.presentation_changed();
                    }
                });
            }
            Host::Window { .. } => self.settle_layout(true),
        }
        true
    }

    fn attach_overlay(
        self: &Rc<Self>,
        overlay: gtk::Overlay,
        origin: Option<gtk::graphene::Rect>,
    ) -> Host {
        let layer = ExpandedLayer::new();
        layer.set_origin(origin);
        layer.set_child(Some(&self.pane));
        self.pane.set_overflow(gtk::Overflow::Hidden);
        let weak = Rc::downgrade(self);
        layer.connect_scrim_pressed(move || {
            if let Some(state) = weak.upgrade() {
                state.collapse(None);
            }
        });
        overlay.add_overlay(&layer);
        Host::Overlay { layer, overlay }
    }

    /// A separate fullscreen window on the main window's monitor. The
    /// compositor animates it; there is no parent, so it is not treated as a dialog.
    fn attach_window(self: &Rc<Self>, main: &gtk::Window) -> Host {
        // A compositor that ignores the fullscreen request still shows a window as big as the main one.
        let window = gtk::Window::builder()
            .title(PREVIEW_LABEL)
            .default_width(main.width().max(MIN_FALLBACK_SIZE))
            .default_height(main.height().max(MIN_FALLBACK_SIZE))
            .build();
        window.add_css_class("expanded-preview-window");
        window.set_child(Some(&self.pane));
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let handler = weak
                .upgrade()
                .and_then(|state| state.expanded.key_handler.borrow().clone());
            handler.map_or(Propagation::Proceed, |handler| handler(key, modifiers))
        });
        window.add_controller(keys);
        let weak = Rc::downgrade(self);
        window.connect_close_request(move |_| {
            if let Some(state) = weak.upgrade() {
                state.collapse_now();
            }
            Propagation::Stop
        });
        let weak = Rc::downgrade(self);
        // The destroy signal only arrives after the last reference is gone.
        let destroyed = main.connect_unrealize(move |_| {
            if let Some(state) = weak.upgrade() {
                state.abandon_expansion();
            }
        });
        if let Some(monitor) = main
            .surface()
            .and_then(|surface| WidgetExt::display(main).monitor_at_surface(&surface))
        {
            window.fullscreen_on_monitor(&monitor);
        } else {
            window.fullscreen();
        }
        window.present();
        Host::Window {
            window,
            main: main.downgrade(),
            destroyed: Rc::new(RefCell::new(Some(destroyed))),
        }
    }

    /// Animates back into the drawer, then runs `then`. A fullscreen window
    /// closes at once and leaves the animation to the compositor.
    fn collapse(self: &Rc<Self>, then: Option<Box<dyn FnOnce()>>) {
        let host = self.expanded.host.borrow().clone();
        let Some(Host::Overlay { layer, overlay }) = host else {
            self.collapse_now();
            if let Some(then) = then {
                then();
            }
            return;
        };
        if self.expanded.collapsing.replace(true) {
            return;
        }
        layer.set_origin(self.drawer_bounds(&overlay));
        let weak = Rc::downgrade(self);
        layer.animate_to(0.0, TRANSITION_MS, move || {
            if let Some(state) = weak.upgrade() {
                state.collapse_now();
            }
            if let Some(then) = then {
                then();
            }
        });
    }

    /// Returns the pane to the drawer immediately.
    pub(super) fn collapse_now(&self) {
        let Some(host) = self.expanded.host.borrow_mut().take() else {
            return;
        };
        self.expanded.collapsing.set(false);
        self.expanded.anchor.set(self.capture_anchor());
        // Pages that rebind in the smaller drawer must not ask for large renders.
        if let Some(view) = self.pdf_view.borrow().as_ref() {
            view.set_detail(PreviewDetail::Standard, self.media_preview_size());
        }
        self.expanded.relocating.set(true);
        match &host {
            Host::Overlay { layer, .. } => {
                layer.cancel_animation();
                layer.take_child();
            }
            Host::Window { window, .. } => window.set_child(None::<&gtk::Widget>),
        }
        self.revealer.set_child(Some(&self.pane));
        self.expanded.relocating.set(false);
        self.release_host(&host);
        self.pane.remove_css_class(EXPANDED_CLASS);
        self.pane.set_overflow(gtk::Overflow::Visible);
        self.sync_expand_button();
        self.set_keyboard_owner(false);
        self.content.set_focusable(false);
        if let Some(browser) = self.sizing.browser() {
            browser.focus_file_view();
        }
        self.content.queue_resize();
        self.settle_layout(true);
    }

    /// Drops the host without moving the pane, for a window that is going away.
    pub(super) fn abandon_expansion(&self) {
        let Some(host) = self.expanded.host.borrow_mut().take() else {
            return;
        };
        self.expanded.collapsing.set(false);
        if let Host::Window { window, .. } = &host {
            window.set_child(None::<&gtk::Widget>);
        }
        self.release_host(&host);
        self.pane.remove_css_class(EXPANDED_CLASS);
    }

    fn release_host(&self, host: &Host) {
        match host {
            Host::Overlay { layer, overlay } => {
                layer.cancel_animation();
                overlay.remove_overlay(layer);
            }
            Host::Window {
                window,
                main,
                destroyed,
            } => {
                if let (Some(main), Some(handler)) = (main.upgrade(), destroyed.borrow_mut().take())
                {
                    main.disconnect(handler);
                }
                window.destroy();
            }
        }
    }

    /// Once the pane has a stable size in its new host, puts the document back
    /// where it was and optionally reports the new presentation.
    fn settle_layout(&self, report: bool) {
        let generation = self.expanded.layout_generation.get().wrapping_add(1);
        self.expanded.layout_generation.set(generation);
        let weak = self.expanded.this.borrow().clone();
        let stable = Cell::new((0u32, 0i32, 0u32));
        self.pane.add_tick_callback(move |pane, _| {
            let Some(state) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if state.expanded.layout_generation.get() != generation {
                return glib::ControlFlow::Break;
            }
            let (frames, width, total) = stable.get();
            let width_now = if pane.is_mapped() { pane.width() } else { 0 };
            let frames = if width_now > 0 && width_now == width {
                frames + 1
            } else {
                0
            };
            stable.set((frames, width_now, total + 1));
            if frames < SETTLE_FRAMES {
                return if total < MAX_SETTLE_FRAMES {
                    glib::ControlFlow::Continue
                } else {
                    glib::ControlFlow::Break
                };
            }
            if let Some(anchor) = state.expanded.anchor.take() {
                state.restore_anchor(anchor);
            }
            if report {
                state.presentation_changed();
            }
            glib::ControlFlow::Break
        });
    }

    fn capture_anchor(&self) -> Option<Anchor> {
        let scroll = self.primary_scroll()?;
        let fraction = |adjustment: gtk::Adjustment| {
            (adjustment.upper() > 0.0)
                .then(|| (adjustment.value() + adjustment.page_size() / 2.0) / adjustment.upper())
        };
        Some(Anchor {
            horizontal: fraction(scroll.hadjustment())?,
            vertical: fraction(scroll.vadjustment())?,
        })
    }

    fn restore_anchor(&self, anchor: Anchor) {
        let Some(scroll) = self.primary_scroll() else {
            return;
        };
        for (adjustment, fraction) in [
            (scroll.hadjustment(), anchor.horizontal),
            (scroll.vadjustment(), anchor.vertical),
        ] {
            set_adjustment_value(
                &adjustment,
                fraction * adjustment.upper() - adjustment.page_size() / 2.0,
            );
        }
    }

    fn drawer_bounds(&self, overlay: &gtk::Overlay) -> Option<gtk::graphene::Rect> {
        if !self.revealer.is_mapped() {
            return None;
        }
        self.revealer.compute_bounds(overlay)
    }

    pub(super) fn sync_expand_button(&self) {
        let expanded = self.expanded.is_active();
        let (icon, tooltip) = if expanded {
            (
                crate::assets::icons::MINIMIZE_2,
                "Collapse preview (Shift+Space)",
            )
        } else {
            (
                crate::assets::icons::MAXIMIZE_2,
                "Expand preview (Shift+Space)",
            )
        };
        crate::assets::set_primary_icon(&self.expand_icon, icon);
        self.expand_button.set_tooltip_text(Some(tooltip));
        self.expand_button
            .update_property(&[gtk::accessible::Property::Label(tooltip)]);
    }
}

mod controls;
mod detail;
#[cfg(test)]
mod probes;
