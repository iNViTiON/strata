// SPDX-License-Identifier: MIT

//! The open PDF viewer's zoom and page state, so keys can drive what the
//! pointer already can: Ctrl+wheel zoom and scrolling.

use std::{cell::Cell, cell::RefCell, collections::HashMap, rc::Rc, sync::Arc};

use gtk::{glib, prelude::*};

use super::{
    PDF_MAX_ZOOM, PDF_MIN_ZOOM, PDF_PAGE_GAP, TEXT_BYTE_LIMIT, pdf_page_width,
    preserve_pdf_view_center, resize_pdf_pages, set_adjustment_value, set_pdf_page_texture,
};
use crate::{
    model::FileEntry,
    services::{
        LoadHandle, MediaPreviewSize, PdfTextLayer, Preview, PreviewContent, PreviewDetail,
        PreviewEvent, PreviewProvider, PreviewRequest, PreviewRequestId,
    },
};

pub(super) type PdfPages =
    Rc<RefCell<HashMap<i32, (gtk::Overlay, gtk::Picture, gtk::DrawingArea)>>>;

const TOP_TOLERANCE: f64 = 4.0;

/// How the pages are being rendered: the size and detail of the next request.
#[derive(Clone, Copy)]
pub(super) struct PdfRender {
    pub(super) size: MediaPreviewSize,
    pub(super) detail: PreviewDetail,
}

#[derive(Clone)]
pub(super) struct PdfView {
    scroll: glib::WeakRef<gtk::ScrolledWindow>,
    zoom: Rc<Cell<f64>>,
    page_width: Rc<Cell<i32>>,
    pages: PdfPages,
    render: Rc<Cell<PdfRender>>,
    refine: Rc<dyn Fn()>,
}

/// What re-rendering the open pages needs to share with the viewer.
pub(super) struct PageRefinement {
    pub(super) render: Rc<Cell<PdfRender>>,
    pub(super) next_request: Rc<Cell<u64>>,
    pub(super) pages: PdfPages,
    pub(super) layers: Rc<RefCell<HashMap<i32, Arc<PdfTextLayer>>>>,
    pub(super) page_width: Rc<Cell<i32>>,
    pub(super) loads: Rc<RefCell<HashMap<i32, LoadHandle>>>,
}

/// Re-requests every page on screen at the current render detail and swaps
/// each texture in place, so a page never goes blank while it sharpens.
pub(super) fn page_refiner(
    provider: Rc<dyn PreviewProvider>,
    entry: FileEntry,
    state: PageRefinement,
) -> Rc<dyn Fn()> {
    Rc::new(move || {
        let visible: Vec<_> = state
            .pages
            .borrow()
            .iter()
            .map(|(page, (overlay, picture, area))| {
                (
                    *page,
                    overlay.clone(),
                    picture.downgrade(),
                    area.downgrade(),
                )
            })
            .collect();
        for (page, overlay, picture, area) in visible {
            let request_id = PreviewRequestId(state.next_request.get());
            state
                .next_request
                .set(state.next_request.get().saturating_add(1));
            let binding = overlay.widget_name();
            let weak_overlay = overlay.downgrade();
            let (layers, loads, page_width) = (
                state.layers.clone(),
                state.loads.clone(),
                state.page_width.clone(),
            );
            let emit = Rc::new(move |event| {
                let PreviewEvent::Ready(Preview {
                    request_id: response,
                    content:
                        PreviewContent::Pdf {
                            png,
                            page: rendered,
                            text_layer,
                            ..
                        },
                    ..
                }) = event
                else {
                    return;
                };
                if response != request_id || rendered != page {
                    return;
                }
                loads.borrow_mut().remove(&page);
                let Some(overlay) = weak_overlay
                    .upgrade()
                    .filter(|overlay| overlay.widget_name() == binding)
                else {
                    return;
                };
                if let Some(layer) = text_layer {
                    layers.borrow_mut().insert(page, layer);
                    if let Some(area) = area.upgrade() {
                        area.queue_draw();
                    }
                }
                if let Some(picture) = picture.upgrade() {
                    set_pdf_page_texture(&overlay, &picture, png, page_width.get());
                }
            });
            let render = state.render.get();
            let load = provider.load(
                PreviewRequest {
                    id: request_id,
                    entry: entry.clone(),
                    text_byte_limit: TEXT_BYTE_LIMIT,
                    render_document: false,
                    pdf_page: page,
                    media_size: render.size,
                    detail: render.detail,
                    model_palette: crate::ui::theme::ThemeManager::shared().active_model_palette(),
                    archive_password: None,
                },
                emit,
            );
            state.loads.borrow_mut().insert(page, load);
        }
    })
}

impl PdfView {
    pub(super) fn new(
        scroll: &gtk::ScrolledWindow,
        zoom: Rc<Cell<f64>>,
        page_width: Rc<Cell<i32>>,
        pages: PdfPages,
        render: Rc<Cell<PdfRender>>,
        refine: Rc<dyn Fn()>,
    ) -> Self {
        let weak = glib::WeakRef::new();
        weak.set(Some(scroll));
        Self {
            scroll: weak,
            zoom,
            page_width,
            pages,
            render,
            refine,
        }
    }

    /// Renders later pages, and sharpens the ones on screen, at `detail`.
    /// Going back to standard detail leaves the pages as they are.
    pub(super) fn set_detail(&self, detail: PreviewDetail, size: MediaPreviewSize) {
        if self.render.get().detail == detail {
            return;
        }
        self.render.set(PdfRender { size, detail });
        if detail.is_expanded() {
            (self.refine)();
        }
    }

    #[cfg(test)]
    pub(super) fn first_page_texture_width(&self) -> Option<i32> {
        let pages = self.pages.borrow();
        let first = pages.keys().min()?;
        let (_, picture, _) = pages.get(first)?;
        Some(picture.paintable()?.intrinsic_width())
    }

    pub(super) fn zoom(&self) -> f64 {
        self.zoom.get()
    }

    /// Sets the zoom, keeping the middle of the view where it is.
    pub(super) fn set_zoom(&self, zoom: f64) -> bool {
        let Some(scroll) = self.scroll.upgrade() else {
            return false;
        };
        let previous = self.zoom.get();
        let next = zoom.clamp(PDF_MIN_ZOOM, PDF_MAX_ZOOM);
        if (next - previous).abs() < f64::EPSILON {
            return false;
        }
        self.zoom.set(next);
        let width = pdf_page_width(&scroll, next);
        self.page_width.set(width);
        resize_pdf_pages(&self.pages.borrow(), width);
        preserve_pdf_view_center(&scroll, next / previous);
        true
    }

    pub(super) fn reset_zoom(&self) {
        let Some(scroll) = self.scroll.upgrade() else {
            return;
        };
        self.zoom.set(PDF_MIN_ZOOM);
        let width = pdf_page_width(&scroll, PDF_MIN_ZOOM);
        self.page_width.set(width);
        resize_pdf_pages(&self.pages.borrow(), width);
        set_adjustment_value(&scroll.hadjustment(), 0.0);
    }

    /// Scrolls to the top of the next or previous page. `false` when no page
    /// is laid out yet.
    pub(super) fn turn_page(&self, direction: i32) -> bool {
        let Some(scroll) = self.scroll.upgrade() else {
            return false;
        };
        let Some(list) = scroll.child() else {
            return false;
        };
        let mut pages: Vec<(f64, f64)> = self
            .pages
            .borrow()
            .values()
            .filter_map(|(overlay, ..)| {
                let bounds = overlay.compute_bounds(&list)?;
                Some((f64::from(bounds.y()), f64::from(bounds.height())))
            })
            .collect();
        pages.sort_by(|a, b| a.0.total_cmp(&b.0));
        let gap = f64::from(PDF_PAGE_GAP);
        let adjustment = scroll.vadjustment();
        let value = adjustment.value();
        let target = if direction > 0 {
            pages
                .iter()
                .find(|(top, _)| *top > value + TOP_TOLERANCE)
                .map(|(top, _)| *top)
                .or_else(|| pages.last().map(|(top, height)| top + height + gap))
        } else {
            let current = pages
                .iter()
                .rev()
                .find(|(top, _)| *top <= value + TOP_TOLERANCE);
            current.map(|(top, height)| {
                if value > top + TOP_TOLERANCE {
                    *top
                } else {
                    (top - height - gap).max(0.0)
                }
            })
        };
        let Some(target) = target else {
            return false;
        };
        set_adjustment_value(&adjustment, target);
        true
    }
}
