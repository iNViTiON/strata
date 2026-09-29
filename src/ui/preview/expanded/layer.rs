// SPDX-License-Identifier: MIT

//! The overlay host for the expanded preview: a scrim with one inset card.

use std::cell::{Cell, RefCell};

use gtk::{glib, graphene, gsk, prelude::*, subclass::prelude::*};

use crate::ui::motion;

const MIN_MARGIN: i32 = 16;
const MAX_MARGIN: i32 = 64;
const MARGIN_RATIO: f64 = 0.05;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct ExpandedLayer {
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
        }

        fn dispose(&self) {
            if let Some(child) = self.child.take() {
                child.unparent();
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
            let Some(child) = self.child.borrow().clone() else {
                return;
            };
            let margin = (f64::from(width.min(height)) * MARGIN_RATIO)
                .round()
                .clamp(f64::from(MIN_MARGIN), f64::from(MAX_MARGIN))
                as i32;
            let (minimum_width, ..) = child.measure(gtk::Orientation::Horizontal, -1);
            let card_width = (width - margin * 2).max(minimum_width).min(width);
            let (minimum_height, ..) = child.measure(gtk::Orientation::Vertical, card_width);
            let card_height = (height - margin * 2).max(minimum_height).min(height);
            let x = (width - card_width) / 2;
            let y = (height - card_height) / 2;
            child.allocate(
                card_width,
                card_height,
                -1,
                Some(gsk::Transform::new().translate(&graphene::Point::new(x as f32, y as f32))),
            );
            self.card.set(Some(graphene::Rect::new(
                x as f32,
                y as f32,
                card_width as f32,
                card_height as f32,
            )));
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let Some(child) = self.child.borrow().clone() else {
                return;
            };
            let layer = self.obj();
            let progress = self.progress.get();
            let (Some(card), true) = (self.card.get(), progress < 1.0) else {
                layer.snapshot_child(&child, snapshot);
                return;
            };
            let from = self.origin.get().unwrap_or(card);
            let mix = |from: f32, to: f32| from + (to - from) * progress as f32;
            snapshot.push_clip(&graphene::Rect::new(
                mix(from.x(), card.x()),
                mix(from.y(), card.y()),
                mix(from.width(), card.width()),
                mix(from.height(), card.height()),
            ));
            layer.snapshot_child(&child, snapshot);
            snapshot.pop();
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
            old.unparent();
        }
        if let Some(child) = child {
            child.set_parent(self);
            slot.replace(Some(child.clone().upcast()));
        }
    }

    pub fn take_child(&self) -> Option<gtk::Widget> {
        let child = self.imp().child.take()?;
        child.unparent();
        Some(child)
    }

    pub fn set_origin(&self, origin: Option<graphene::Rect>) {
        self.imp().origin.set(origin);
    }

    /// The card area, in the layer's coordinates.
    pub fn card(&self) -> Option<graphene::Rect> {
        self.imp().card.get()
    }

    pub fn progress(&self) -> f64 {
        self.imp().progress.get()
    }

    fn set_progress(&self, progress: f64) {
        let progress = progress.clamp(0.0, 1.0);
        self.imp().progress.set(progress);
        self.set_opacity(progress);
        self.queue_draw();
    }

    /// Runs the open (`1.0`) or close (`0.0`) transition, then `done`.
    /// A newer call supersedes an unfinished one without running its `done`.
    pub fn animate_to(&self, target: f64, duration_ms: f64, done: impl FnOnce() + 'static) {
        let imp = self.imp();
        let generation = imp.generation.get().wrapping_add(1);
        imp.generation.set(generation);
        let from = self.progress();
        let displayed = self.parent().is_some_and(|parent| parent.is_mapped());
        if !motion::animations_enabled() || !displayed || (from - target).abs() < 1e-3 {
            self.set_progress(target);
            done();
            return;
        }
        let start = Cell::new(None::<i64>);
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
            let elapsed_ms = (now - begin) as f64 / 1000.0;
            let time = (elapsed_ms / duration_ms).min(1.0);
            layer.set_progress(from + (target - from) * motion::emphasized_deceleration(time));
            if time < 1.0 {
                return glib::ControlFlow::Continue;
            }
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
