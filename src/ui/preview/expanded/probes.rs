// SPDX-License-Identifier: MIT

//! Test-only views of the expanded preview's state.

use super::*;

impl PreviewDrawer {
    pub(in crate::ui) fn pane_widget(&self) -> gtk::Widget {
        self.state.pane.clone().upcast()
    }

    pub(in crate::ui) fn content_widget(&self) -> gtk::Widget {
        self.state.content.clone().upcast()
    }
}

impl PreviewDrawer {
    /// The scrolled document's sideways position and how far it can go.
    pub(in crate::ui) fn document_pan(&self) -> Option<(f64, f64)> {
        let adjustment = self.state.primary_scroll()?.hadjustment();
        Some((
            adjustment.value(),
            (adjustment.upper() - adjustment.page_size()).max(0.0),
        ))
    }

    pub(in crate::ui) fn image_zoom(&self) -> Option<f64> {
        self.state
            .zoom_view
            .borrow()
            .as_ref()
            .map(zoom::ZoomPicture::zoom_level)
    }

    pub(in crate::ui) fn image_is_zoomed(&self) -> bool {
        self.state
            .zoom_view
            .borrow()
            .as_ref()
            .is_some_and(zoom::ZoomPicture::is_zoomed)
    }

    pub(in crate::ui) fn image_is_laid_out(&self) -> bool {
        self.state
            .zoom_view
            .borrow()
            .as_ref()
            .is_some_and(|picture| picture.width() > 0 && picture.height() > 0)
    }

    pub(in crate::ui) fn image_texture_size(&self) -> Option<(i32, i32)> {
        self.state
            .zoom_view
            .borrow()
            .as_ref()
            .map(zoom::ZoomPicture::texture_size)
    }

    /// Pixel width of the first page on screen.
    pub(in crate::ui) fn pdf_page_texture_width(&self) -> Option<i32> {
        self.state
            .pdf_view
            .borrow()
            .as_ref()?
            .first_page_texture_width()
    }

    pub(in crate::ui) fn image_center(&self) -> Option<(f64, f64)> {
        self.state
            .zoom_view
            .borrow()
            .as_ref()
            .map(zoom::ZoomPicture::center)
    }

    pub(in crate::ui) fn pdf_zoom(&self) -> Option<f64> {
        self.state
            .pdf_view
            .borrow()
            .as_ref()
            .map(pdf_view::PdfView::zoom)
    }

    /// The scrolled document's middle as a fraction of its height, and that height.
    pub(in crate::ui) fn document_position(&self) -> Option<(f64, f64)> {
        let adjustment = self.state.primary_scroll()?.vadjustment();
        (adjustment.upper() > 0.0).then(|| {
            (
                (adjustment.value() + adjustment.page_size() / 2.0) / adjustment.upper(),
                adjustment.upper(),
            )
        })
    }

    /// The line of text at the middle of the view.
    pub(in crate::ui) fn text_center_line(&self) -> Option<i32> {
        let view = self.state.text_view.borrow().clone()?;
        let visible = view.visible_rect();
        let iter = view.iter_at_location(
            visible.x() + visible.width() / 2,
            visible.y() + visible.height() / 2,
        )?;
        Some(iter.line())
    }

    pub(in crate::ui) fn set_document_fraction(&self, fraction: f64) {
        if let Some(scroll) = self.state.primary_scroll() {
            let adjustment = scroll.vadjustment();
            set_adjustment_value(
                &adjustment,
                fraction * adjustment.upper() - adjustment.page_size() / 2.0,
            );
        }
    }

    /// The header button's action, without a pointer.
    pub(in crate::ui) fn toggle_expanded_from_button(&self) {
        self.state.expand_button.emit_clicked();
    }

    pub(in crate::ui) fn expanded_window(&self) -> Option<gtk::Window> {
        match self.state.expanded.host.borrow().as_ref() {
            Some(Host::Window { window, .. }) => Some(window.clone()),
            _ => None,
        }
    }
}
