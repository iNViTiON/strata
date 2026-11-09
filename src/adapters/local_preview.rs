// SPDX-License-Identifier: MIT

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    rc::Rc,
};

mod load;
mod remote;

use futures_channel::oneshot;
use gtk::{gio, glib, prelude::*};
use load::{PreviewLoad, RenderFn};

use crate::{
    adapters::gio_close::Closing,
    sandbox::{Cancellation, MediaPreviewBackend, PdfRenderSize},
    services::{
        LoadHandle, MediaPreviewSize, ModelRender, PreviewContent, PreviewDetail, PreviewEvent,
        PreviewPriority, PreviewProvider, PreviewRequest, normalize_preview_text,
        route_preview::RouteRender,
    },
};
// The test modules reach these through `super::*`.
#[cfg(test)]
use crate::{
    sandbox::ParseOperation,
    services::{
        ModelFormat, Preview, SandboxedMedia,
        route_preview::{RouteFormat, RoutePreview},
    },
};

const MAX_PREVIEW_CACHE_ENTRIES: usize = 64;
const MAX_PREVIEW_CACHE_BYTES: usize = 128 * 1024 * 1024;
const MAX_CONCURRENT_HEAVY_PREVIEWS: usize = 1;

/// Lower ranks are served first. A neighbor never overtakes interactive work
/// but does run before the queued later pages of the document being read.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum HeavyRank {
    Interactive,
    Neighbor,
    LaterPage,
}

#[derive(Default)]
struct HeavyPreviewQueue {
    running: usize,
    next_id: u64,
    queued: VecDeque<(u64, HeavyRank, oneshot::Sender<HeavyPreviewPermit>)>,
    // Cancels the neighbor render holding the permit, so interactive work never waits for it.
    neighbor_running: Option<Cancellation>,
}

struct HeavyPreviewWaiter {
    id: u64,
    receive: Option<oneshot::Receiver<HeavyPreviewPermit>>,
}

impl HeavyPreviewWaiter {
    async fn acquire(mut self) -> Option<HeavyPreviewPermit> {
        self.receive.take()?.await.ok()
    }
}

impl Drop for HeavyPreviewWaiter {
    fn drop(&mut self) {
        HEAVY_PREVIEW_QUEUE.with(|queue| {
            queue.borrow_mut().queued.retain(|(id, ..)| *id != self.id);
        });
    }
}

struct HeavyPreviewPermit;

impl Drop for HeavyPreviewPermit {
    fn drop(&mut self) {
        release_heavy_preview_permit();
    }
}

fn request_heavy_preview_permit() -> HeavyPreviewWaiter {
    request_ranked_heavy_permit(HeavyRank::Interactive)
}

fn request_ranked_heavy_permit(rank: HeavyRank) -> HeavyPreviewWaiter {
    let (send, receive) = oneshot::channel();
    let (id, start) = HEAVY_PREVIEW_QUEUE.with(|queue| {
        let mut queue = queue.borrow_mut();
        let id = queue.next_id;
        queue.next_id = queue.next_id.saturating_add(1);
        if queue.running < MAX_CONCURRENT_HEAVY_PREVIEWS {
            queue.running += 1;
            (id, Some(send))
        } else {
            if rank == HeavyRank::Interactive
                && let Some(neighbor) = &queue.neighbor_running
            {
                neighbor.cancel();
            }
            queue.queued.push_back((id, rank, send));
            (id, None)
        }
    });
    if let Some(send) = start
        && let Err(permit) = send.send(HeavyPreviewPermit)
    {
        drop(permit);
    }
    HeavyPreviewWaiter {
        id,
        receive: Some(receive),
    }
}

fn release_heavy_preview_permit() {
    let next = HEAVY_PREVIEW_QUEUE.with(|queue| {
        let mut queue = queue.borrow_mut();
        queue.running = queue.running.saturating_sub(1);
        queue.neighbor_running = None;
        let next = queue
            .queued
            .iter()
            .enumerate()
            .min_by_key(|(_, (id, rank, _))| (*rank, *id))
            .map(|(index, _)| index)
            .and_then(|index| queue.queued.remove(index))
            .map(|(_, _, send)| send);
        if next.is_some() {
            queue.running += 1;
        }
        next
    });
    if let Some(send) = next
        && let Err(permit) = send.send(HeavyPreviewPermit)
    {
        drop(permit);
    }
}

/// A neighbor's heavy render, which the interactive request for the same
/// page joins rather than repeating.
struct NeighborRender {
    token: u64,
    running: bool,
    cancel: Cancellation,
    waiters: Vec<oneshot::Sender<()>>,
}

struct NeighborRenderGuard {
    key: PreviewCacheKey,
    token: u64,
}

impl Drop for NeighborRenderGuard {
    fn drop(&mut self) {
        let finished = NEIGHBOR_RENDERS.with(|renders| {
            let mut renders = renders.borrow_mut();
            renders
                .get(&self.key)
                .is_some_and(|render| render.token == self.token)
                .then(|| renders.remove(&self.key))
                .flatten()
        });
        for waiter in finished.into_iter().flat_map(|render| render.waiters) {
            let _ = waiter.send(());
        }
    }
}

fn begin_neighbor_render(
    key: &PreviewCacheKey,
    cancel: &Cancellation,
) -> Option<NeighborRenderGuard> {
    NEIGHBOR_RENDERS.with(|renders| {
        let mut renders = renders.borrow_mut();
        if renders.contains_key(key) {
            return None;
        }
        let token = NEXT_NEIGHBOR_TOKEN.with(|next| next.replace(next.get().wrapping_add(1)));
        renders.insert(
            key.clone(),
            NeighborRender {
                token,
                running: false,
                cancel: cancel.clone(),
                waiters: Vec::new(),
            },
        );
        Some(NeighborRenderGuard {
            key: key.clone(),
            token,
        })
    })
}

fn mark_neighbor_running(key: &PreviewCacheKey, cancel: &Cancellation) {
    NEIGHBOR_RENDERS.with(|renders| {
        if let Some(render) = renders.borrow_mut().get_mut(key) {
            render.running = true;
        }
    });
    HEAVY_PREVIEW_QUEUE.with(|queue| queue.borrow_mut().neighbor_running = Some(cancel.clone()));
}

/// An interactive request for a page a neighbor is rendering waits for it; if
/// the neighbor has not started, it is dropped and the request renders itself.
fn join_neighbor_render(key: &PreviewCacheKey) -> Option<oneshot::Receiver<()>> {
    NEIGHBOR_RENDERS.with(|renders| {
        let mut renders = renders.borrow_mut();
        let render = renders.get_mut(key)?;
        if !render.running {
            render.cancel.cancel();
            return None;
        }
        let (send, receive) = oneshot::channel();
        render.waiters.push(send);
        Some(receive)
    })
}

struct PreviewCache {
    entries: HashMap<PreviewCacheKey, PreviewContent>,
    recent: VecDeque<PreviewCacheKey>,
    byte_count: usize,
}

#[derive(Clone, Eq, Hash, PartialEq)]
struct PreviewCacheKey {
    path: PathBuf,
    modified: i64,
    pdf_page: Option<(i32, PdfRenderSize)>,
    model: Option<ModelRender>,
    route: Option<RouteRender>,
    /// PDF pages already key their render size; images differ only by this.
    expanded: bool,
}

impl PreviewCache {
    fn get(&mut self, key: &PreviewCacheKey) -> Option<PreviewContent> {
        let content = self.entries.get(key)?.clone();
        self.recent.retain(|k| k != key);
        self.recent.push_back(key.clone());
        Some(content)
    }

    fn insert(&mut self, key: PreviewCacheKey, content: PreviewContent) {
        if matches!(content, PreviewContent::SandboxedMedia { .. }) {
            return;
        }
        let bytes = preview_content_size(&content);
        self.recent.retain(|k| k != &key);
        if let Some(old) = self.entries.remove(&key) {
            self.byte_count = self.byte_count.saturating_sub(preview_content_size(&old));
        }
        self.byte_count = self.byte_count.saturating_add(bytes);
        self.recent.push_back(key.clone());
        self.entries.insert(key, content);
        while self.entries.len() > MAX_PREVIEW_CACHE_ENTRIES
            || self.byte_count > MAX_PREVIEW_CACHE_BYTES
        {
            let Some(oldest) = self.recent.pop_front() else {
                break;
            };
            if let Some(removed) = self.entries.remove(&oldest) {
                self.byte_count = self
                    .byte_count
                    .saturating_sub(preview_content_size(&removed));
            }
        }
    }
}

fn preview_content_size(content: &PreviewContent) -> usize {
    match content {
        PreviewContent::Route { preview } => {
            preview.png.len()
                + preview.profile.as_ref().map_or(0, Vec::len)
                + preview.samples.len() * size_of::<crate::services::route_preview::RouteSample>()
        }
        PreviewContent::Rasterized { png } | PreviewContent::Model { png, .. } => png.len(),
        PreviewContent::Pdf {
            png, text_layer, ..
        } => {
            png.len()
                + text_layer.as_ref().map_or(0, |layer| {
                    layer.text.len() + layer.glyphs.len() * size_of::<[f32; 4]>()
                })
        }
        PreviewContent::SandboxedMedia { .. } => 0,
        PreviewContent::Text { content, .. } => content.len(),
        _ => 0,
    }
}

thread_local! {
    static HEAVY_PREVIEW_QUEUE: RefCell<HeavyPreviewQueue> = RefCell::new(HeavyPreviewQueue::default());
    static NEIGHBOR_RENDERS: RefCell<HashMap<PreviewCacheKey, NeighborRender>> = RefCell::new(HashMap::new());
    static NEXT_NEIGHBOR_TOKEN: Cell<u64> = const { Cell::new(0) };
    static PREVIEW_CACHE: RefCell<PreviewCache> = RefCell::new(PreviewCache {
        entries: HashMap::new(),
        recent: VecDeque::new(),
        byte_count: 0,
    });
}

pub struct LocalPreviewProvider {
    media_preview_backend: Rc<dyn Fn() -> MediaPreviewBackend>,
}

impl LocalPreviewProvider {
    pub(crate) fn new(media_preview_backend: Rc<dyn Fn() -> MediaPreviewBackend>) -> Self {
        Self {
            media_preview_backend,
        }
    }
}

impl PreviewProvider for LocalPreviewProvider {
    fn preload(&self, request: PreviewRequest, emit: Rc<dyn Fn(PreviewEvent)>) -> LoadHandle {
        self.load_prioritized(
            request,
            emit,
            crate::sandbox::parse_neighbor,
            PreviewPriority::Neighbor,
        )
    }

    fn load(&self, request: PreviewRequest, emit: Rc<dyn Fn(PreviewEvent)>) -> LoadHandle {
        use futures_lite::StreamExt;
        if !crate::services::is_model(&request.entry) {
            return self.load_with_renderer(request, emit, crate::sandbox::parse);
        }
        let (send, mut receive) = futures_channel::mpsc::channel(8);
        let send = RefCell::new(send);
        let request_id = request.id;
        let progress_emit = emit.clone();
        let progress_task = glib::MainContext::default().spawn_local(async move {
            while let Some(stage) = receive.next().await {
                progress_emit(PreviewEvent::Progress { request_id, stage });
            }
        });
        let load = self.load_with_renderer(
            request,
            emit,
            move |path, operation, value, backend, cancellation| {
                let request = crate::sandbox::ParseRequest {
                    input: path,
                    operation,
                    value,
                    media_backend: backend,
                    cancellation,
                    priority: PreviewPriority::Current,
                };
                crate::sandbox::parse_with_progress(request, &|stage| {
                    let _ = send.borrow_mut().try_send(stage);
                })
            },
        );
        LoadHandle::new(move || {
            drop(load);
            progress_task.abort();
        })
    }
}

impl LocalPreviewProvider {
    fn load_with_renderer(
        &self,
        request: PreviewRequest,
        emit: Rc<dyn Fn(PreviewEvent)>,
        render: impl RenderFn,
    ) -> LoadHandle {
        self.load_prioritized(request, emit, render, PreviewPriority::Current)
    }

    fn load_prioritized(
        &self,
        request: PreviewRequest,
        emit: Rc<dyn Fn(PreviewEvent)>,
        render: impl RenderFn,
        priority: PreviewPriority,
    ) -> LoadHandle {
        let media_preview_backend = (self.media_preview_backend)();
        let cancellation = Cancellation::default();
        let abort_safe = Rc::new(Cell::new(true));
        let load = PreviewLoad::new(
            request,
            emit,
            media_preview_backend,
            cancellation.clone(),
            abort_safe.clone(),
            priority == PreviewPriority::Neighbor,
        );
        let task = glib::MainContext::default().spawn_local(load.run(render));
        LoadHandle::new(move || {
            cancellation.cancel();
            if abort_safe.get() {
                task.abort();
            }
        })
    }
}

fn pdf_render_size(viewport: MediaPreviewSize, detail: PreviewDetail) -> PdfRenderSize {
    match detail {
        PreviewDetail::Standard => PdfRenderSize::for_viewport_width(viewport.width),
        PreviewDetail::Expanded { width } => PdfRenderSize::for_expanded_width(width),
    }
}

async fn read_text(
    file: &gio::File,
    native_path: Option<&Path>,
    byte_limit: usize,
) -> Result<(String, bool), glib::Error> {
    if let Some(path) = native_path {
        let path = path.to_path_buf();
        let result = gio::spawn_blocking(move || {
            use std::io::Read;
            let io_failure = |error: std::io::Error| {
                glib::Error::new(
                    gio::IOErrorEnum::Failed,
                    &crate::services::io_error_message(&error),
                )
            };
            let file = crate::services::file_activity::open(&path).map_err(io_failure)?;
            let mut bytes = Vec::new();
            file.take(byte_limit as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(io_failure)?;
            let truncated = bytes.len() > byte_limit;
            let sample = &bytes[..bytes.len().min(byte_limit)];
            Ok((decode_text_sample(sample), truncated))
        })
        .await;
        match result {
            Ok(ok) => return ok,
            Err(_) => {
                return Err(glib::Error::new(
                    gio::IOErrorEnum::Cancelled,
                    "Read cancelled",
                ));
            }
        }
    }
    let stream = gio::GioFuture::new(file, |file, cancellable, result| {
        file.read_async(glib::Priority::DEFAULT, Some(cancellable), move |output| {
            result.resolve(output.map(|stream| Closing::new(stream.upcast::<gio::InputStream>())));
        });
    })
    .await?;
    let bytes = stream
        .read_bytes_future(byte_limit.saturating_add(1), glib::Priority::DEFAULT)
        .await?;
    let bytes = bytes.as_ref();
    let truncated = bytes.len() > byte_limit;
    let sample = &bytes[..bytes.len().min(byte_limit)];
    Ok((decode_text_sample(sample), truncated))
}

fn decode_text_sample(sample: &[u8]) -> String {
    let decoded = match sample {
        [0xFF, 0xFE, body @ ..] => decode_utf16(body, u16::from_le_bytes),
        [0xFE, 0xFF, body @ ..] => decode_utf16(body, u16::from_be_bytes),
        _ => String::from_utf8_lossy(sample).into_owned(),
    };
    normalize_preview_text(&decoded).into_owned()
}

fn decode_utf16(body: &[u8], unit: fn([u8; 2]) -> u16) -> String {
    let units: Vec<u16> = body
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| unit(*pair))
        .collect();
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
mod tests;
