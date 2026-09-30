// SPDX-License-Identifier: MIT

use gtk::{gdk, glib, graphene, gsk, prelude::*, subclass::prelude::*};

pub(super) const MAX_CONTENT_WIDTH: i32 = 1280;
const MAX_UPSCALE: f64 = 2.0;
const MEDIA_MARGIN: i32 = 12;

/// The expanded preview lifts the width cap and lets video fill its host.
pub(super) fn in_expanded_preview(widget: &gtk::Widget) -> bool {
    std::iter::successors(widget.parent(), |parent| parent.parent())
        .any(|ancestor| ancestor.has_css_class(super::expanded::EXPANDED_CLASS))
}

fn fitted_size(
    width: i32,
    height: i32,
    intrinsic_width: i32,
    intrinsic_height: i32,
    max_upscale: f64,
) -> (i32, i32) {
    let width = width.max(0);
    let height = height.max(0);
    if intrinsic_width <= 0 || intrinsic_height <= 0 {
        return (width, height);
    }
    let scale = (f64::from(width) / f64::from(intrinsic_width))
        .min(f64::from(height) / f64::from(intrinsic_height))
        .min(max_upscale);
    (
        (f64::from(intrinsic_width) * scale).floor() as i32,
        (f64::from(intrinsic_height) * scale).floor() as i32,
    )
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct MediaLayout {
        pub paintable: glib::WeakRef<gdk::Paintable>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MediaLayout {
        const NAME: &'static str = "StrataPreviewMediaLayout";
        type Type = super::MediaLayout;
        type ParentType = gtk::LayoutManager;
    }

    impl ObjectImpl for MediaLayout {}

    impl LayoutManagerImpl for MediaLayout {
        fn request_mode(&self, _: &gtk::Widget) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(
            &self,
            widget: &gtk::Widget,
            orientation: gtk::Orientation,
            for_size: i32,
        ) -> (i32, i32, i32, i32) {
            let mut minimum = 0;
            let mut natural = 0;
            let content_width = if in_expanded_preview(widget) {
                i32::MAX
            } else {
                MAX_CONTENT_WIDTH
            };
            let mut child = widget.first_child().and_then(|media| media.next_sibling());
            while let Some(control) = child {
                if control.should_layout() {
                    let (min, nat, _, _) =
                        control.measure(orientation, for_size.min(content_width));
                    if orientation == gtk::Orientation::Horizontal {
                        minimum = minimum.max(min);
                        natural = natural.max(nat);
                    } else {
                        minimum += min;
                        natural += nat;
                    }
                }
                child = control.next_sibling();
            }
            (minimum, natural, -1, -1)
        }

        fn allocate(&self, widget: &gtk::Widget, width: i32, height: i32, _: i32) {
            let Some(media) = widget.first_child() else {
                return;
            };
            let expanded = in_expanded_preview(widget);
            let section_width = if expanded {
                width
            } else {
                width.min(MAX_CONTENT_WIDTH)
            };
            let mut controls = Vec::new();
            let mut controls_height = 0;
            let mut child = media.next_sibling();
            while let Some(control) = child {
                if control.should_layout() {
                    let height = control.measure(gtk::Orientation::Vertical, section_width).1;
                    controls_height += height;
                    controls.push((control.clone(), height));
                }
                child = control.next_sibling();
            }
            let media_height = (height - controls_height).max(0);
            let paintable = self.paintable.upgrade();
            let zoomable = media.downcast_ref::<super::super::zoom::ZoomPicture>();
            let (intrinsic_width, intrinsic_height) = match zoomable {
                Some(picture) => picture.texture_size(),
                None => paintable
                    .as_ref()
                    .map_or((0, 0), |p| (p.intrinsic_width(), p.intrinsic_height())),
            };
            let video = paintable
                .as_ref()
                .is_some_and(|p| p.is::<crate::ui::media::DecodedMedia>());
            let (fitted_width, fitted_height) = fitted_size(
                section_width - MEDIA_MARGIN * 2,
                media_height - MEDIA_MARGIN * 2,
                intrinsic_width,
                intrinsic_height,
                if expanded && video {
                    f64::INFINITY
                } else {
                    MAX_UPSCALE
                },
            );
            let (outer_width, outer_height) = if expanded && zoomable.is_some() {
                (section_width, media_height)
            } else {
                (
                    (fitted_width + MEDIA_MARGIN * 2).min(section_width),
                    (fitted_height + MEDIA_MARGIN * 2).min(media_height),
                )
            };
            allocate_at(
                &media,
                outer_width,
                outer_height,
                (width - outer_width) / 2,
                (media_height - outer_height) / 2,
            );
            let mut y = media_height;
            for (control, height) in controls {
                allocate_at(
                    &control,
                    section_width,
                    height,
                    (width - section_width) / 2,
                    y,
                );
                y += height;
            }
        }
    }
}

fn allocate_at(widget: &gtk::Widget, width: i32, height: i32, x: i32, y: i32) {
    widget.allocate(
        width,
        height,
        -1,
        Some(gsk::Transform::new().translate(&graphene::Point::new(x as f32, y as f32))),
    );
}

glib::wrapper! {
    pub struct MediaLayout(ObjectSubclass<imp::MediaLayout>) @extends gtk::LayoutManager;
}

pub(super) fn section(
    media: &impl IsA<gtk::Widget>,
    paintable: &impl IsA<gdk::Paintable>,
) -> gtk::Box {
    let section = gtk::Box::new(gtk::Orientation::Vertical, 0);
    section.set_hexpand(true);
    section.set_vexpand(true);
    let layout: MediaLayout = glib::Object::new();
    layout.imp().paintable.set(Some(paintable.as_ref()));
    section.set_layout_manager(Some(layout));
    media.set_margin_start(MEDIA_MARGIN);
    media.set_margin_end(MEDIA_MARGIN);
    media.set_margin_top(MEDIA_MARGIN);
    media.set_margin_bottom(MEDIA_MARGIN);
    section.append(media);
    section
}
