// SPDX-License-Identifier: MIT

//! The overlay host for the expanded preview: a scrim with one card that
//! grows out of the drawer and shrinks back into it.

use std::cell::{Cell, OnceCell, RefCell};

use gtk::{glib, graphene, gsk, prelude::*, subclass::prelude::*};

use crate::ui::motion;

const MIN_MARGIN: i32 = 16;
const MAX_MARGIN: i32 = 64;
const MARGIN_RATIO: f64 = 0.05;
/// Where a card with no drawer to leave from starts, relative to its final size.
const FALLBACK_SCALE: f32 = 0.94;
const OPEN_CLASS: &str = "open";

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct ExpandedLayer {
        pub(super) scrim: OnceCell<gtk::Widget>,
        pub(super) child: RefCell<Option<gtk::Widget>>,
        pub(super) progress: Cell<f64>,
        pub(super) origin: Cell<Option<graphene::Rect>>,
        pub(super) card: Cell<Option<graphene::Rect>>,
        pub(super) generation: Cell<u64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ExpandedLayer {
        const NAME: &'static str = "StrataExpandedPreviewLayer";
        type Type = super::ExpandedLayer;
        type ParentType = gtk::Widget;

        fn class_init(class: &mut Self::Class) {
            class.set_css_name("expanded-preview-layer");
        }
    }

    impl ObjectImpl for ExpandedLayer {
        fn constructed(&self) {
            self.parent_constructed();
            let layer = self.obj();
            layer.add_css_class("expanded-preview-layer");
            layer.set_halign(gtk::Align::Fill);
            layer.set_valign(gtk::Align::Fill);
            layer.set_hexpand(true);
            layer.set_vexpand(true);
            layer.set_overflow(gtk::Overflow::Hidden);
            let scrim = gtk::Box::new(gtk::Orientation::Vertical, 0);
            scrim.add_css_class("expanded-preview-scrim");
            scrim.set_parent(&*layer);
            let _ = self.scrim.set(scrim.upcast());
        }

        fn dispose(&self) {
            if let Some(child) = self.child.take() {
                child.unparent();
            }
            if let Some(scrim) = self.scrim.get() {
                scrim.unparent();
            }
        }
    }

    impl WidgetImpl for ExpandedLayer {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::ConstantSize
        }

        fn measure(&self, _: gtk::Orientation, _: i32) -> (i32, i32, i32, i32) {
            (0, 0, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, _: i32) {
            if let Some(scrim) = self.scrim.get() {
                scrim.measure(gtk::Orientation::Horizontal, -1);
                scrim.measure(gtk::Orientation::Vertical, width);
                scrim.allocate(width, height, -1, None);
            }
            let Some(child) = self.child.borrow().clone() else {
                return;
            };
            let rect = self.rect_now(&child, width, height);
            child.allocate(
                rect.width() as i32,
                rect.height() as i32,
                -1,
                Some(gsk::Transform::new().translate(&graphene::Point::new(rect.x(), rect.y()))),
            );
            self.card.set(Some(rect));
        }
    }

    impl ExpandedLayer {
        /// The card's final area, inset from the layer's edges.
        fn resting_rect(child: &gtk::Widget, width: i32, height: i32) -> graphene::Rect {
            let margin = (f64::from(width.min(height)) * MARGIN_RATIO)
                .round()
                .clamp(f64::from(MIN_MARGIN), f64::from(MAX_MARGIN))
                as i32;
            let (minimum_width, ..) = child.measure(gtk::Orientation::Horizontal, -1);
            let card_width = (width - margin * 2).max(minimum_width).min(width);
            let (minimum_height, ..) = child.measure(gtk::Orientation::Vertical, card_width);
            let card_height = (height - margin * 2).max(minimum_height).min(height);
            graphene::Rect::new(
                ((width - card_width) / 2) as f32,
                ((height - card_height) / 2) as f32,
                card_width as f32,
                card_height as f32,
            )
        }

        /// The card between the drawer and its final area, on whole pixels so
        /// the last frame lines up with the drawer exactly.
        fn rect_now(&self, child: &gtk::Widget, width: i32, height: i32) -> graphene::Rect {
            let resting = Self::resting_rect(child, width, height);
            let from = self.origin.get().unwrap_or_else(|| {
                let (shrunk_width, shrunk_height) = (
                    resting.width() * FALLBACK_SCALE,
                    resting.height() * FALLBACK_SCALE,
                );
                graphene::Rect::new(
                    resting.x() + (resting.width() - shrunk_width) / 2.0,
                    resting.y() + (resting.height() - shrunk_height) / 2.0,
                    shrunk_width,
                    shrunk_height,
                )
            });
            let progress = self.progress.get() as f32;
            let mix = |from: f32, to: f32| (from + (to - from) * progress).round();
            let (minimum_width, ..) = child.measure(gtk::Orientation::Horizontal, -1);
            let card_width = (mix(from.width(), resting.width()) as i32)
                .max(minimum_width)
                .min(width);
            let (minimum_height, ..) = child.measure(gtk::Orientation::Vertical, card_width);
            let card_height = (mix(from.height(), resting.height()) as i32)
                .max(minimum_height)
                .min(height);
            graphene::Rect::new(
                (mix(from.x(), resting.x()) as i32).clamp(0, width - card_width) as f32,
                (mix(from.y(), resting.y()) as i32).clamp(0, height - card_height) as f32,
                card_width as f32,
                card_height as f32,
            )
        }
    }
}

glib::wrapper! {
    pub struct ExpandedLayer(ObjectSubclass<imp::ExpandedLayer>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl ExpandedLayer {
    pub fn new() -> Self {
        let layer: Self = glib::Object::new();
        layer.set_progress(0.0);
        layer
    }

    pub fn set_child(&self, child: Option<&impl IsA<gtk::Widget>>) {
        let slot = &self.imp().child;
        if let Some(old) = slot.take() {
            old.set_opacity(1.0);
            old.unparent();
        }
        if let Some(child) = child {
            child.set_parent(self);
            slot.replace(Some(child.clone().upcast()));
        }
        self.apply_progress();
    }

    pub fn take_child(&self) -> Option<gtk::Widget> {
        let child = self.imp().child.take()?;
        child.set_opacity(1.0);
        child.unparent();
        Some(child)
    }

    /// Where the card grows from and shrinks back to, in the layer's
    /// coordinates. Without one it scales and fades in place.
    pub fn set_origin(&self, origin: Option<graphene::Rect>) {
        self.imp().origin.set(origin);
        self.apply_progress();
    }

    /// Whether the card has shrunk all the way into its origin.
    pub fn rests_in_origin(&self) -> bool {
        let imp = self.imp();
        imp.origin.get().is_some() && imp.progress.get() <= 0.0
    }

    /// The card's area right now, in the layer's coordinates.
    pub fn card(&self) -> Option<graphene::Rect> {
        self.imp().card.get()
    }

    pub(super) fn progress(&self) -> f64 {
        self.imp().progress.get()
    }

    fn set_progress(&self, progress: f64) {
        self.imp().progress.set(progress.clamp(0.0, 1.0));
        self.apply_progress();
    }

    /// The card stays opaque while it moves, so the drawer is never left
    /// empty; only the scrim fades. A card with no drawer fades itself.
    fn apply_progress(&self) {
        let imp = self.imp();
        let progress = imp.progress.get();
        if let Some(scrim) = imp.scrim.get() {
            scrim.set_opacity(progress);
        }
        if let Some(child) = imp.child.borrow().as_ref() {
            let fades = imp.origin.get().is_none();
            child.set_opacity(if fades { progress } else { 1.0 });
        }
        self.queue_allocate();
    }

    fn set_open(&self, open: bool) {
        if open {
            self.add_css_class(OPEN_CLASS);
        } else {
            self.remove_css_class(OPEN_CLASS);
        }
    }

    /// Runs the open (`1.0`) or close (`0.0`) transition, then `done`.
    /// A newer call supersedes an unfinished one without running its `done`.
    pub fn animate_to(&self, target: f64, duration_ms: f64, done: impl FnOnce() + 'static) {
        let imp = self.imp();
        let generation = imp.generation.get().wrapping_add(1);
        imp.generation.set(generation);
        let from = self.progress();
        let open = target > 0.5;
        let displayed = self.parent().is_some_and(|parent| parent.is_mapped());
        if !motion::animations_enabled() || !displayed || (from - target).abs() < 1e-3 {
            self.set_progress(target);
            self.set_open(open);
            done();
            return;
        }
        if !open {
            self.set_open(false);
        }
        let start = Cell::new(None::<i64>);
        let frames = Cell::new(0u32);
        let done = RefCell::new(Some(done));
        self.add_tick_callback(move |layer, clock| {
            if layer.imp().generation.get() != generation {
                return glib::ControlFlow::Break;
            }
            let now = clock.frame_time();
            let begin = start.get().unwrap_or_else(|| {
                start.set(Some(now));
                now
            });
            // The pane's styling changes on the second frame, once it has a
            // resolved style in this layer to transition away from.
            frames.set(frames.get() + 1);
            if open && frames.get() == 2 {
                layer.set_open(true);
            }
            let elapsed_ms = (now - begin) as f64 / 1000.0;
            let time = (elapsed_ms / duration_ms).min(1.0);
            if time < 1.0 {
                layer.set_progress(from + (target - from) * motion::emphasized(time));
                return glib::ControlFlow::Continue;
            }
            layer.set_progress(target);
            layer.set_open(open);
            if let Some(done) = done.borrow_mut().take() {
                done();
            }
            glib::ControlFlow::Break
        });
    }

    /// Calls `handler` when the scrim around the card is pressed.
    pub fn connect_scrim_pressed(&self, handler: impl Fn() + 'static) {
        let click = gtk::GestureClick::new();
        click.connect_pressed(glib::clone!(
            #[weak(rename_to = layer)]
            self,
            move |_, _, x, y| {
                let outside = layer.card().is_some_and(|card| {
                    !card.contains_point(&graphene::Point::new(x as f32, y as f32))
                });
                if outside {
                    handler();
                }
            }
        ));
        self.add_controller(click);
    }

    pub fn cancel_animation(&self) {
        let imp = self.imp();
        imp.generation.set(imp.generation.get().wrapping_add(1));
    }
}
