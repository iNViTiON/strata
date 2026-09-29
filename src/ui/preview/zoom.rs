// SPDX-License-Identifier: MIT

//! Image previews. A picture fits its area like `gtk::Picture`; while the
//! preview is expanded it also zooms and pans, and keeps that view when it
//! returns to the drawer and expands again.

use std::cell::{Cell, RefCell};

use gtk::{gdk, glib, graphene, gsk, prelude::*, subclass::prelude::*};

pub(super) const MAX_ZOOM: f64 = 8.0;
const MAX_UPSCALE: f64 = 2.0;
const KEY_ZOOM_STEP: f64 = 1.25;
const WHEEL_ZOOM_RATE: f64 = 0.14;
const PAN_FRACTION: f64 = 0.15;
const ZOOMED: f64 = 1.001;

mod imp {
    use super::*;

    pub struct ZoomPicture {
        pub(super) texture: RefCell<Option<gdk::Texture>>,
        pub(super) zoom: Cell<f64>,
        /// The image point at the middle of the view, as fractions of its size.
        pub(super) center: Cell<(f64, f64)>,
        pub(super) pointer: Cell<(f64, f64)>,
        pub(super) drag_from: Cell<(f64, f64)>,
    }

    impl Default for ZoomPicture {
        fn default() -> Self {
            Self {
                texture: RefCell::new(None),
                zoom: Cell::new(1.0),
                center: Cell::new((0.5, 0.5)),
                pointer: Cell::new((0.0, 0.0)),
                drag_from: Cell::new((0.5, 0.5)),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ZoomPicture {
        const NAME: &'static str = "StrataZoomPicture";
        type Type = super::ZoomPicture;
        type ParentType = gtk::Widget;

        fn class_init(class: &mut Self::Class) {
            class.set_css_name("zoom-picture");
            class.set_accessible_role(gtk::AccessibleRole::Img);
        }
    }

    impl ObjectImpl for ZoomPicture {
        fn constructed(&self) {
            self.parent_constructed();
            let picture = self.obj();
            picture.add_css_class("preview-zoom");
            picture.set_overflow(gtk::Overflow::Hidden);
            picture.set_hexpand(true);
            picture.set_vexpand(true);
            picture.install_controllers();
        }
    }

    impl WidgetImpl for ZoomPicture {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::ConstantSize
        }

        fn measure(&self, orientation: gtk::Orientation, _: i32) -> (i32, i32, i32, i32) {
            let natural = self.texture.borrow().as_ref().map_or(0, |texture| {
                if orientation == gtk::Orientation::Horizontal {
                    texture.width()
                } else {
                    texture.height()
                }
            });
            (0, natural, -1, -1)
        }

        fn size_allocate(&self, _: i32, _: i32, _: i32) {
            self.obj().clamp_center();
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let picture = self.obj();
            let Some(texture) = self.texture.borrow().clone() else {
                return;
            };
            let Some(view) = picture.view() else {
                return;
            };
            let (width, height) = (picture.width() as f32, picture.height() as f32);
            let filter = if view.scale < 1.0 {
                gsk::ScalingFilter::Trilinear
            } else {
                gsk::ScalingFilter::Linear
            };
            snapshot.push_clip(&graphene::Rect::new(0.0, 0.0, width, height));
            snapshot.append_scaled_texture(
                &texture,
                filter,
                &graphene::Rect::new(
                    view.x as f32,
                    view.y as f32,
                    view.width as f32,
                    view.height as f32,
                ),
            );
            snapshot.pop();
        }
    }
}

glib::wrapper! {
    pub struct ZoomPicture(ObjectSubclass<imp::ZoomPicture>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

/// Where the image is drawn inside the widget, in widget pixels.
struct View {
    scale: f64,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl ZoomPicture {
    pub fn new(texture: &gdk::Texture) -> Self {
        let picture: Self = glib::Object::new();
        picture.imp().texture.replace(Some(texture.clone()));
        picture
    }

    /// Swaps the pixels and keeps the zoom and the point being looked at.
    pub fn set_texture(&self, texture: &gdk::Texture) {
        self.imp().texture.replace(Some(texture.clone()));
        self.queue_resize();
        self.queue_draw();
    }

    pub fn texture_size(&self) -> (i32, i32) {
        self.imp()
            .texture
            .borrow()
            .as_ref()
            .map_or((0, 0), |texture| (texture.width(), texture.height()))
    }

    /// Whether the image is magnified in the current view.
    pub fn is_zoomed(&self) -> bool {
        self.effective_zoom() > ZOOMED
    }

    #[cfg(test)]
    pub fn center(&self) -> (f64, f64) {
        self.imp().center.get()
    }

    #[cfg(test)]
    pub fn zoom_level(&self) -> f64 {
        self.imp().zoom.get()
    }

    pub fn zoom_by(&self, factor: f64) {
        let (width, height) = (f64::from(self.width()), f64::from(self.height()));
        self.zoom_at(factor, width / 2.0, height / 2.0);
    }

    pub fn zoom_in(&self) {
        self.zoom_by(KEY_ZOOM_STEP);
    }

    pub fn zoom_out(&self) {
        self.zoom_by(1.0 / KEY_ZOOM_STEP);
    }

    pub fn reset(&self) {
        let imp = self.imp();
        imp.zoom.set(1.0);
        imp.center.set((0.5, 0.5));
        self.queue_draw();
    }

    /// Moves the view by a fraction of its size. `false` when the image fits.
    pub fn pan(&self, horizontal: f64, vertical: f64) -> bool {
        if !self.is_zoomed() {
            return false;
        }
        let Some(view) = self.view() else {
            return false;
        };
        let (cx, cy) = self.imp().center.get();
        let (width, height) = (f64::from(self.width()), f64::from(self.height()));
        self.imp().center.set((
            cx + horizontal * PAN_FRACTION * width / view.width,
            cy + vertical * PAN_FRACTION * height / view.height,
        ));
        self.clamp_center();
        self.queue_draw();
        true
    }

    fn effective_zoom(&self) -> f64 {
        if super::media_layout::in_expanded_preview(self.upcast_ref()) {
            self.imp().zoom.get()
        } else {
            1.0
        }
    }

    fn view(&self) -> Option<View> {
        let (texture_width, texture_height) = self.texture_size();
        let (width, height) = (f64::from(self.width()), f64::from(self.height()));
        if texture_width <= 0 || texture_height <= 0 || width <= 0.0 || height <= 0.0 {
            return None;
        }
        let (texture_width, texture_height) = (f64::from(texture_width), f64::from(texture_height));
        let fit = (width / texture_width)
            .min(height / texture_height)
            .min(MAX_UPSCALE);
        let scale = fit * self.effective_zoom();
        let (view_width, view_height) = (texture_width * scale, texture_height * scale);
        let (cx, cy) = self.imp().center.get();
        let place = |extent: f64, size: f64, center: f64| {
            if size <= extent {
                (extent - size) / 2.0
            } else {
                (extent / 2.0 - center * size).clamp(extent - size, 0.0)
            }
        };
        Some(View {
            scale,
            x: place(width, view_width, cx),
            y: place(height, view_height, cy),
            width: view_width,
            height: view_height,
        })
    }

    fn clamp_center(&self) {
        let Some(view) = self.view() else {
            return;
        };
        let (width, height) = (f64::from(self.width()), f64::from(self.height()));
        let limit = |extent: f64, size: f64, center: f64| {
            if size <= extent {
                0.5
            } else {
                let margin = extent / 2.0 / size;
                center.clamp(margin, 1.0 - margin)
            }
        };
        let (cx, cy) = self.imp().center.get();
        self.imp()
            .center
            .set((limit(width, view.width, cx), limit(height, view.height, cy)));
    }

    /// Zooms while the image point under (`x`, `y`) stays where it is.
    fn zoom_at(&self, factor: f64, x: f64, y: f64) {
        let imp = self.imp();
        let next = (imp.zoom.get() * factor).clamp(1.0, MAX_ZOOM);
        if (next - imp.zoom.get()).abs() < f64::EPSILON {
            return;
        }
        let Some(before) = self.view() else {
            imp.zoom.set(next);
            self.queue_draw();
            return;
        };
        let anchor = (
            (x - before.x) / before.width,
            (y - before.y) / before.height,
        );
        imp.zoom.set(next);
        let Some(after) = self.view() else {
            return;
        };
        let (width, height) = (f64::from(self.width()), f64::from(self.height()));
        imp.center.set((
            (width / 2.0 - (x - anchor.0 * after.width)) / after.width,
            (height / 2.0 - (y - anchor.1 * after.height)) / after.height,
        ));
        self.clamp_center();
        self.queue_draw();
    }

    fn install_controllers(&self) {
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(glib::clone!(
            #[weak(rename_to = picture)]
            self,
            move |_, x, y| picture.imp().pointer.set((x, y))
        ));
        self.add_controller(motion);

        let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        wheel.set_propagation_phase(gtk::PropagationPhase::Capture);
        wheel.connect_scroll(glib::clone!(
            #[weak(rename_to = picture)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |controller, _, dy| {
                let ctrl = controller
                    .current_event_state()
                    .contains(gdk::ModifierType::CONTROL_MASK);
                if !ctrl || !super::media_layout::in_expanded_preview(picture.upcast_ref()) {
                    return glib::Propagation::Proceed;
                }
                let (x, y) = picture.imp().pointer.get();
                picture.zoom_at((-dy * WHEEL_ZOOM_RATE).exp(), x, y);
                glib::Propagation::Stop
            }
        ));
        self.add_controller(wheel);

        // Added before the file drag source so a zoomed image pans instead of dragging.
        let drag = gtk::GestureDrag::new();
        drag.set_button(1);
        drag.set_propagation_phase(gtk::PropagationPhase::Capture);
        drag.connect_drag_begin(glib::clone!(
            #[weak(rename_to = picture)]
            self,
            move |gesture, _, _| {
                if picture.is_zoomed() {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    picture.set_cursor_from_name(Some("grabbing"));
                    picture.imp().drag_from.set(picture.imp().center.get());
                }
            }
        ));
        drag.connect_drag_update(glib::clone!(
            #[weak(rename_to = picture)]
            self,
            move |_, dx, dy| {
                if !picture.is_zoomed() {
                    return;
                }
                let Some(view) = picture.view() else {
                    return;
                };
                let (cx, cy) = picture.imp().drag_from.get();
                picture
                    .imp()
                    .center
                    .set((cx - dx / view.width, cy - dy / view.height));
                picture.clamp_center();
                picture.queue_draw();
            }
        ));
        drag.connect_drag_end(glib::clone!(
            #[weak(rename_to = picture)]
            self,
            move |_, _, _| picture.set_cursor_from_name(Some("grab"))
        ));
        self.add_controller(drag);
    }
}
