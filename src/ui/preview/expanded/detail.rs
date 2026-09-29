// SPDX-License-Identifier: MIT

//! Sharper rasters for the expanded view, swapped in without touching zoom or scroll.

use super::*;

const EXPANDED_FALLBACK_WIDTH: i32 = 1_600;

impl PreviewState {
    /// The decode size that fits the expanded view, once it has a size.
    pub(in crate::ui::preview) fn expanded_media_size(&self) -> Option<MediaPreviewSize> {
        if !self.expanded.is_active() {
            return None;
        }
        let (width, height) = (self.content.width(), self.content.height());
        (width > 0 && height > 0)
            .then(|| MediaPreviewSize::for_viewport(width, height, self.pane.scale_factor().max(1)))
    }

    /// The detail rasters need right now: larger while the view is expanded.
    pub(in crate::ui::preview) fn current_detail(&self) -> PreviewDetail {
        if !self.expanded.is_active() || self.expanded.collapsing.get() {
            return PreviewDetail::Standard;
        }
        let logical = match self.pane.width() {
            width if width > 0 => width,
            _ => self
                .pane
                .root()
                .map_or(EXPANDED_FALLBACK_WIDTH, |root| root.width()),
        };
        PreviewDetail::Expanded {
            width: logical.saturating_mul(self.pane.scale_factor().max(1)),
        }
    }

    /// Once the way the preview is presented has settled, sharper rasters
    /// replace the drawer-sized ones in place, without touching zoom or scroll.
    pub(in crate::ui::preview) fn install_presentation_refinement(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.on_presentation_changed(move || {
            if let Some(state) = weak.upgrade() {
                state.refine_presentation();
            }
        });
    }

    fn refine_presentation(self: &Rc<Self>) {
        let detail = self.current_detail();
        self.refine_image(detail);
        if let Some(view) = self.pdf_view.borrow().as_ref() {
            view.set_detail(detail, self.media_preview_size());
        }
    }

    /// Requests a larger raster of the open image and swaps only its texture.
    fn refine_image(self: &Rc<Self>, detail: PreviewDetail) {
        if !detail.is_expanded() || self.loaded_detail.get().is_expanded() {
            return;
        }
        let Some(picture) = self.zoom_view.borrow().clone() else {
            return;
        };
        let Some(entry) = self.current.borrow().clone() else {
            return;
        };
        if entry.location.native_path().is_none()
            || crate::services::is_model(&entry.native_name)
            || crate::sandbox::CoverFormat::for_name(&entry.native_name).is_some()
        {
            return;
        }
        let request_id = PreviewRequestId(self.next_request.get());
        self.next_request
            .set(self.next_request.get().saturating_add(1));
        let weak = Rc::downgrade(self);
        let weak_picture = picture.downgrade();
        let target = entry.clone();
        let emit = Rc::new(move |event| {
            let PreviewEvent::Ready(Preview {
                request_id: response,
                content: PreviewContent::Rasterized { png },
                ..
            }) = event
            else {
                return;
            };
            let (Some(state), Some(picture)) = (weak.upgrade(), weak_picture.upgrade()) else {
                return;
            };
            let unchanged = response == request_id
                && state.current.borrow().as_ref() == Some(&target)
                && state.zoom_view.borrow().as_ref() == Some(&picture);
            if !unchanged {
                return;
            }
            if let Ok(texture) = gtk::gdk::Texture::from_bytes(&glib::Bytes::from_owned(png)) {
                picture.set_texture(&texture);
                state.loaded_detail.set(detail);
            }
        });
        let load = self.provider.load(
            PreviewRequest {
                id: request_id,
                entry,
                text_byte_limit: TEXT_BYTE_LIMIT,
                render_document: false,
                pdf_page: 0,
                media_size: self.media_preview_size(),
                detail,
                model_palette: crate::ui::theme::ThemeManager::shared().active_model_palette(),
                archive_password: None,
            },
            emit,
        );
        self.refine_load.replace(Some(load));
    }
}
