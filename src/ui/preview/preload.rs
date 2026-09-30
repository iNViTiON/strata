// SPDX-License-Identifier: MIT

//! Neighbor previews: with the preview open, the entries displayed directly
//! above and below the selection are prepared through the provider's
//! low-priority `preload` path, so moving onto one needs no new decode.
//!
//! Images and PDF pages are rendered into the preview cache. Video and audio
//! become parked [`DecodedMedia`] streams: the sandboxed worker decodes the
//! first frame, blocks on its full pipe, and is promoted when selected.

use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
    time::{Duration, Instant},
};

use gtk::{glib, prelude::*};

use crate::{
    model::{FileEntry, MetadataValue},
    sandbox::media::Session,
    services::{
        LoadHandle, MediaPreviewSize, PreloadKind, PreviewContent, PreviewDetail, PreviewEvent,
        PreviewRequest, PreviewRequestId, SandboxedMedia, preload_kind,
    },
    ui::media::DecodedMedia,
};

use super::{PreviewState, TEXT_BYTE_LIMIT, is_audio_type, preview_target};

const NEIGHBORS: usize = 2;
const START_DELAY: Duration = Duration::from_millis(150);
const RETRY_DELAY: Duration = Duration::from_millis(250);
const MAX_RETRIES: u32 = 24;
// Selection changes closer together than this are scrolling, not choosing.
const RAPID_FOCUS: Duration = Duration::from_millis(150);

#[derive(Default)]
struct Shared {
    ready: Cell<bool>,
    failed: Cell<bool>,
    closed: Cell<bool>,
    media: RefCell<Option<DecodedMedia>>,
}

struct Slot {
    entry: FileEntry,
    kind: PreloadKind,
    size: MediaPreviewSize,
    // Only whether it is expanded matters: expanded renders do not depend on the width.
    expanded: bool,
    shared: Rc<Shared>,
    _load: LoadHandle,
}

impl Slot {
    // A parked stream is promoted at whatever size is asked for and then resized,
    // so only other kinds depend on the request's size and detail.
    fn matches(&self, entry: &FileEntry, size: MediaPreviewSize, detail: PreviewDetail) -> bool {
        same_file(&self.entry, entry)
            && (self.kind == PreloadKind::Media
                || (self.size == size && self.expanded == detail.is_expanded()))
    }

    fn is_failed(&self) -> bool {
        self.shared.failed.get()
            || self
                .shared
                .media
                .borrow()
                .as_ref()
                .is_some_and(|media| media.error().is_some())
    }

    fn is_ready(&self) -> bool {
        !self.shared.failed.get()
            && (self.shared.ready.get()
                || self
                    .shared
                    .media
                    .borrow()
                    .as_ref()
                    .is_some_and(DecodedMedia::is_parked_ready))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.shared.closed.set(true);
        if let Some(media) = self.shared.media.borrow_mut().take() {
            media.close();
        }
    }
}

fn same_file(a: &FileEntry, b: &FileEntry) -> bool {
    a.location == b.location && a.modified_unix_seconds == b.modified_unix_seconds
}

enum Candidate {
    Prepare(PreloadKind),
    Skip,
    /// Listings fill in modification times later, and without one a result
    /// could not be matched to the file that produced it.
    AwaitingModifiedTime,
}

fn candidate(entry: &FileEntry) -> Candidate {
    let Some(entry) = preview_target(Some(entry.clone())) else {
        return Candidate::Skip;
    };
    if entry.location.native_path().is_none() {
        return Candidate::Skip;
    }
    let Some(kind) = preload_kind(&entry.native_name) else {
        return Candidate::Skip;
    };
    match entry.modified_unix_seconds {
        MetadataValue::Known(_) => Candidate::Prepare(kind),
        MetadataValue::Unknown | MetadataValue::Unavailable => Candidate::AwaitingModifiedTime,
    }
}

#[derive(Default)]
pub(super) struct NeighborPreload {
    owner: RefCell<Weak<PreviewState>>,
    enabled: Cell<bool>,
    slots: RefCell<Vec<Slot>>,
    timer: RefCell<Option<glib::SourceId>>,
    retries: Cell<u32>,
    last_focus: Cell<Option<Instant>>,
}

impl NeighborPreload {
    pub(super) fn attach(&self, owner: Weak<PreviewState>) {
        self.owner.replace(owner);
    }

    pub(super) fn set_enabled(&self, enabled: bool) {
        self.enabled.set(enabled);
        if enabled {
            crate::ui::media::prewarm_audio();
        } else {
            self.clear();
        }
    }

    pub(super) fn clear(&self) {
        if let Some(timer) = self.timer.borrow_mut().take() {
            timer.remove();
        }
        self.slots.borrow_mut().clear();
    }

    /// Records a selection change; `true` while the selection is moving quickly.
    pub(super) fn note_focus_change(&self) -> bool {
        let now = Instant::now();
        self.last_focus
            .replace(Some(now))
            .is_some_and(|previous| now.duration_since(previous) < RAPID_FOCUS)
    }

    /// Whether `entry` can be shown from work that is already done.
    pub(super) fn is_ready(
        &self,
        entry: &FileEntry,
        size: MediaPreviewSize,
        detail: PreviewDetail,
    ) -> bool {
        self.enabled.get()
            && self
                .slots
                .borrow()
                .iter()
                .any(|slot| slot.matches(entry, size, detail) && slot.is_ready())
    }

    /// Takes the parked stream for `source`, promoted and ready to present.
    pub(super) fn take_media(
        &self,
        source: &SandboxedMedia,
        entry: &FileEntry,
    ) -> Option<DecodedMedia> {
        let slot = {
            let mut slots = self.slots.borrow_mut();
            let index = slots.iter().position(|slot| {
                same_file(&slot.entry, entry)
                    && slot
                        .shared
                        .media
                        .borrow()
                        .as_ref()
                        .is_some_and(|media| media.decodes(source))
            })?;
            slots.remove(index)
        };
        let media = slot.shared.media.borrow_mut().take()?;
        if media.promote() {
            media.resize(source.size);
            tracing::debug!("parked neighbor preview promoted");
            Some(media)
        } else {
            tracing::debug!("parked neighbor preview could not be promoted");
            media.close();
            None
        }
    }

    /// Starts neighbor work once the current preview has settled.
    pub(super) fn schedule(&self) {
        self.retries.set(0);
        self.arm(START_DELAY);
    }

    fn arm(&self, delay: Duration) {
        if !self.enabled.get() {
            return;
        }
        if let Some(timer) = self.timer.borrow_mut().take() {
            timer.remove();
        }
        let owner = self.owner.borrow().clone();
        let source = glib::timeout_add_local_once(delay, move || {
            let Some(state) = owner.upgrade() else {
                return;
            };
            state.preload.timer.borrow_mut().take();
            state.preload.run(&state);
        });
        self.timer.replace(Some(source));
    }

    fn run(&self, state: &Rc<PreviewState>) {
        if !self.enabled.get()
            || !state.is_enabled()
            || state.sizing.is_suspended()
            || state.current.borrow().is_none()
        {
            return;
        }
        let Some(browser) = state.sizing.browser() else {
            return;
        };
        let Some((focused, neighbors)) = browser.focused_with_neighbors() else {
            self.slots.borrow_mut().clear();
            return;
        };
        // The selection moved on since this was scheduled; its own event reschedules.
        if state
            .current
            .borrow()
            .as_ref()
            .is_none_or(|current| current.location != focused.location)
        {
            return;
        }
        // The open animation still changes the size neighbors are made for.
        if state.animating.get() {
            self.arm(START_DELAY);
            return;
        }
        if crate::sandbox::browser::thumbnails_pending() {
            if self.retries.get() < MAX_RETRIES {
                self.retries.set(self.retries.get() + 1);
                self.arm(RETRY_DELAY);
            }
            return;
        }
        if self.reconcile(state, neighbors, state.media_preview_size())
            && self.retries.get() < MAX_RETRIES
        {
            self.retries.set(self.retries.get() + 1);
            self.arm(RETRY_DELAY);
        }
    }

    /// Makes the slots match the eligible neighbors: matching work is kept,
    /// everything else is dropped, and missing work is started. `true` while a
    /// neighbor is waiting for its modification time.
    pub(super) fn reconcile(
        &self,
        state: &Rc<PreviewState>,
        neighbors: [Option<FileEntry>; 2],
        size: MediaPreviewSize,
    ) -> bool {
        if !self.enabled.get() {
            return false;
        }
        let [previous, next] = neighbors;
        let mut waiting = false;
        let wanted: Vec<(FileEntry, PreloadKind)> = [next, previous]
            .into_iter()
            .flatten()
            .filter_map(|entry| match candidate(&entry) {
                Candidate::Prepare(kind) => Some((entry, kind)),
                Candidate::Skip => None,
                Candidate::AwaitingModifiedTime => {
                    waiting = true;
                    None
                }
            })
            .take(NEIGHBORS)
            .collect();
        // A failed neighbor is retried on the next settle rather than kept.
        let detail = state.current_detail();
        self.slots.borrow_mut().retain(|slot| {
            !slot.is_failed()
                && wanted
                    .iter()
                    .any(|(entry, _)| slot.matches(entry, size, detail))
        });
        for (entry, kind) in wanted {
            let known = self
                .slots
                .borrow()
                .iter()
                .any(|slot| slot.matches(&entry, size, detail));
            if known || (kind == PreloadKind::Media && !Session::preload_available()) {
                continue;
            }
            tracing::debug!(?kind, "neighbor preview requested");
            let slot = Self::start(state, entry, kind, size, detail);
            self.slots.borrow_mut().push(slot);
        }
        waiting
    }

    fn start(
        state: &Rc<PreviewState>,
        entry: FileEntry,
        kind: PreloadKind,
        size: MediaPreviewSize,
        detail: PreviewDetail,
    ) -> Slot {
        let size = if kind == PreloadKind::Media {
            neighbor_media_size(size)
        } else {
            size
        };
        let shared = Rc::new(Shared::default());
        let weak = Rc::downgrade(&shared);
        let emit = Rc::new(move |event: PreviewEvent| {
            let Some(shared) = weak.upgrade().filter(|shared| !shared.closed.get()) else {
                return;
            };
            match event {
                PreviewEvent::Ready(preview) => match preview.content {
                    PreviewContent::SandboxedMedia { mut media }
                        if Session::preload_available() =>
                    {
                        media.audio_only = is_audio_type(&preview.content_type);
                        shared.media.replace(Some(DecodedMedia::preload(media)));
                    }
                    PreviewContent::Rasterized { .. } | PreviewContent::Pdf { .. } => {
                        shared.ready.set(true);
                    }
                    _ => shared.failed.set(true),
                },
                PreviewEvent::Failed { .. } | PreviewEvent::NeedsPassword { .. } => {
                    shared.failed.set(true);
                }
                PreviewEvent::Progress { .. } => {}
            }
        });
        let id = PreviewRequestId(state.next_request.get());
        state.next_request.set(id.0.saturating_add(1));
        let load = state.provider.preload(
            PreviewRequest {
                id,
                entry: entry.clone(),
                text_byte_limit: TEXT_BYTE_LIMIT,
                render_document: false,
                pdf_page: 0,
                media_size: size,
                detail,
                model_palette: crate::ui::theme::ThemeManager::shared().active_model_palette(),
                archive_password: None,
            },
            emit,
        );
        Slot {
            entry,
            kind,
            size,
            expanded: detail.is_expanded(),
            shared,
            _load: load,
        }
    }
}

#[cfg(test)]
impl NeighborPreload {
    pub(super) fn is_enabled(&self) -> bool {
        self.enabled.get()
    }

    pub(super) fn slot_names(&self) -> Vec<String> {
        let mut names: Vec<_> = self
            .slots
            .borrow()
            .iter()
            .map(|slot| slot.entry.display_name.clone())
            .collect();
        names.sort();
        names
    }

    pub(super) fn parked_stream(&self, name: &str) -> Option<DecodedMedia> {
        self.slots
            .borrow()
            .iter()
            .find(|slot| slot.entry.display_name == name)
            .and_then(|slot| slot.shared.media.borrow().clone())
    }
}

/// Neighbors decode at the drawer's size and rate even while the preview is
/// expanded; the expanded view upgrades the stream by a handover once it is shown.
pub(super) fn neighbor_media_size(size: MediaPreviewSize) -> MediaPreviewSize {
    MediaPreviewSize::new(size.width, size.height)
}
