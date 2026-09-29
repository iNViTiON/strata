// SPDX-License-Identifier: MIT

//! What the arrow, zoom and media keys do to the content of the expanded view.

use super::*;

const SEEK_STEP_US: i64 = 5_000_000;
const SEEK_JUMP_US: i64 = 60_000_000;
const PAN_FRACTION: f64 = 8.0;
const PDF_ZOOM_STEP: f64 = 1.25;

#[derive(Clone, Copy, Eq, PartialEq)]
enum Content {
    Media,
    Archive,
    Image,
    Pdf,
    Document,
    Other,
}

impl PreviewState {
    fn content_kind(&self) -> Content {
        if self.media.borrow().is_some() {
            Content::Media
        } else if self.archive_browser.borrow().is_some() {
            Content::Archive
        } else if self.zoom_view.borrow().is_some() {
            Content::Image
        } else if let Some(scroll) = self.primary_scroll() {
            if scroll.has_css_class("preview-pdf-scroll") {
                Content::Pdf
            } else {
                Content::Document
            }
        } else {
            Content::Other
        }
    }

    pub(super) fn control(&self, arrow: PreviewArrow) -> bool {
        match self.content_kind() {
            Content::Media => {
                let delta = match arrow {
                    PreviewArrow::Left => -SEEK_STEP_US,
                    PreviewArrow::Right => SEEK_STEP_US,
                    PreviewArrow::Up => SEEK_JUMP_US,
                    PreviewArrow::Down => -SEEK_JUMP_US,
                };
                self.seek_media(delta);
                true
            }
            Content::Archive => {
                self.archive_key(match arrow {
                    PreviewArrow::Up => Key::Up,
                    PreviewArrow::Down => Key::Down,
                    PreviewArrow::Left => Key::Left,
                    PreviewArrow::Right => Key::Right,
                });
                true
            }
            Content::Image => {
                let (horizontal, vertical) = match arrow {
                    PreviewArrow::Left => (-1.0, 0.0),
                    PreviewArrow::Right => (1.0, 0.0),
                    PreviewArrow::Up => (0.0, -1.0),
                    PreviewArrow::Down => (0.0, 1.0),
                };
                self.zoom_view
                    .borrow()
                    .as_ref()
                    .is_some_and(|picture| picture.pan(horizontal, vertical))
            }
            Content::Pdf | Content::Document => {
                match arrow {
                    PreviewArrow::Up => {
                        self.scroll_document(DocumentScroll::Line(-1));
                    }
                    PreviewArrow::Down => {
                        self.scroll_document(DocumentScroll::Line(1));
                    }
                    PreviewArrow::Left | PreviewArrow::Right => {
                        let direction = if arrow == PreviewArrow::Left { -1 } else { 1 };
                        let turned = || {
                            self.pdf_view
                                .borrow()
                                .as_ref()
                                .is_some_and(|view| view.turn_page(direction))
                        };
                        if !self.pan_horizontally(direction) && !turned() {
                            self.scroll_document(DocumentScroll::Page(direction));
                        }
                    }
                }
                true
            }
            Content::Other => false,
        }
    }

    pub(super) fn zoom(&self, step: ZoomStep) -> bool {
        if let Some(picture) = self.zoom_view.borrow().as_ref() {
            match step {
                ZoomStep::In => picture.zoom_in(),
                ZoomStep::Out => picture.zoom_out(),
                ZoomStep::Fit => picture.reset(),
            }
            return true;
        }
        let Some(view) = self.pdf_view.borrow().clone() else {
            return false;
        };
        match step {
            ZoomStep::In => view.set_zoom(view.zoom() * PDF_ZOOM_STEP),
            ZoomStep::Out => view.set_zoom(view.zoom() / PDF_ZOOM_STEP),
            ZoomStep::Fit => {
                view.reset_zoom();
                true
            }
        };
        true
    }

    fn seek_media(&self, delta_us: i64) {
        let Some(media) = self.media.borrow().clone() else {
            return;
        };
        if !media.is_seekable() {
            return;
        }
        let end = if media.duration() > 0 {
            media.duration()
        } else {
            i64::MAX
        };
        media.seek((media.timestamp() + delta_us).clamp(0, end));
    }

    fn pan_horizontally(&self, direction: i32) -> bool {
        let Some(scroll) = self.primary_scroll() else {
            return false;
        };
        let adjustment = scroll.hadjustment();
        let limit = adjustment.upper() - adjustment.page_size();
        if limit - adjustment.lower() < 1.0 {
            return false;
        }
        let step = adjustment.page_size() / PAN_FRACTION;
        adjustment.set_value(
            (adjustment.value() + f64::from(direction) * step)
                .clamp(adjustment.lower(), limit.max(adjustment.lower())),
        );
        true
    }
}
