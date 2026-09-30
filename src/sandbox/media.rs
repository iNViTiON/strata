// SPDX-License-Identifier: MIT

use std::{
    io::{self, Read},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use crate::{
    media::{Decoder, FRAME_TIMEOUT, Header, Packet, STARTUP_TIMEOUT, TimedReader},
    services::SandboxedMedia,
};

use super::*;

pub(crate) const MAX_WORKERS: usize = 4;
// Neighbor previews draw on their own budget, so they can never make a
// player the user asked for report "busy".
pub(crate) const MAX_PRELOAD_WORKERS: usize = 2;
const QUEUED_PACKETS: usize = 3;
const ACTIVE_POLL: Duration = Duration::from_millis(10);
// A parked worker has nothing to hand over until promoted; wake it less often.
const PARKED_POLL: Duration = Duration::from_millis(50);
static ACTIVE_WORKERS: AtomicUsize = AtomicUsize::new(0);
static PRELOAD_WORKERS: AtomicUsize = AtomicUsize::new(0);

struct WorkerSlot {
    parked: AtomicBool,
}

impl WorkerSlot {
    fn acquire() -> Option<Self> {
        Self::reserve(&ACTIVE_WORKERS, MAX_WORKERS).then(|| Self {
            parked: AtomicBool::new(false),
        })
    }

    fn acquire_preload() -> Option<Self> {
        Self::reserve(&PRELOAD_WORKERS, MAX_PRELOAD_WORKERS).then(|| Self {
            parked: AtomicBool::new(true),
        })
    }

    fn reserve(counter: &AtomicUsize, limit: usize) -> bool {
        counter
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < limit).then_some(count + 1)
            })
            .is_ok()
    }

    /// Moves the slot onto the interactive budget. Fails, leaving the slot
    /// parked, while all interactive players are taken.
    fn promote(&self) -> bool {
        if !self.parked.load(Ordering::Acquire) {
            return true;
        }
        if !Self::reserve(&ACTIVE_WORKERS, MAX_WORKERS) {
            return false;
        }
        if self.parked.swap(false, Ordering::AcqRel) {
            PRELOAD_WORKERS.fetch_sub(1, Ordering::AcqRel);
        } else {
            ACTIVE_WORKERS.fetch_sub(1, Ordering::AcqRel);
        }
        true
    }
}

impl Drop for WorkerSlot {
    fn drop(&mut self) {
        let parked = self.parked.load(Ordering::Acquire);
        if parked {
            PRELOAD_WORKERS.fetch_sub(1, Ordering::AcqRel);
        } else {
            ACTIVE_WORKERS.fetch_sub(1, Ordering::AcqRel);
        }
        tracing::debug!(
            parked,
            active_workers = ACTIVE_WORKERS.load(Ordering::Acquire),
            preload_workers = PRELOAD_WORKERS.load(Ordering::Acquire),
            "sandboxed media worker stopped"
        );
    }
}

pub(crate) enum Event {
    Prepared(Header),
    Packet(Packet),
    Failed(String),
}

pub(crate) struct Session {
    cancellation: Cancellation,
    receiver: mpsc::Receiver<Event>,
    worker: thread::JoinHandle<()>,
    slot: Arc<WorkerSlot>,
}

impl Session {
    pub fn start(source: SandboxedMedia, start_tick: u32) -> Result<Self, String> {
        let slot = WorkerSlot::acquire().ok_or_else(|| "Media previews are busy (four active players). Pause or close another preview and retry.".to_owned())?;
        Self::spawn(source, start_tick, slot)
    }

    /// A worker for a neighboring file: it decodes its first frames, then
    /// blocks on backpressure until `promote` hands it to a player.
    pub fn start_preload(source: SandboxedMedia, start_tick: u32) -> Result<Self, String> {
        let slot = WorkerSlot::acquire_preload()
            .ok_or_else(|| "Neighbor previews are using every preload worker".to_owned())?;
        Self::spawn(source, start_tick, slot)
    }

    pub fn preload_available() -> bool {
        PRELOAD_WORKERS.load(Ordering::Acquire) < MAX_PRELOAD_WORKERS
    }

    /// Moves a preloaded worker onto the interactive budget without touching
    /// the decoder. `false` means every interactive slot is in use.
    pub fn promote(&self) -> bool {
        self.slot.promote()
    }

    fn spawn(source: SandboxedMedia, start_tick: u32, slot: WorkerSlot) -> Result<Self, String> {
        let slot = Arc::new(slot);
        let worker_slot = slot.clone();
        tracing::debug!(
            active_workers = ACTIVE_WORKERS.load(Ordering::Acquire),
            preload_workers = PRELOAD_WORKERS.load(Ordering::Acquire),
            "sandboxed media worker started"
        );
        let cancellation = Cancellation::default();
        let cancelled = cancellation.clone();
        let (sender, receiver) = mpsc::sync_channel(QUEUED_PACKETS);
        let worker = thread::Builder::new()
            .name("media-preview".into())
            .spawn(move || {
                let slot = worker_slot;
                if let Err(error) = render(&source, start_tick, &cancelled, &sender, &slot.parked) {
                    let _sent = send(&sender, Event::Failed(error), &cancelled, &slot.parked);
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(Self {
            cancellation,
            receiver,
            worker,
            slot,
        })
    }

    pub fn receive(&self) -> Option<Event> {
        match self.receiver.try_recv() {
            Ok(event) => Some(event),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(Event::Failed(
                "The sandboxed media worker stopped unexpectedly".into(),
            )),
        }
    }

    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    pub fn finished(&self) -> bool {
        self.worker.is_finished()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn send(
    sender: &mpsc::SyncSender<Event>,
    mut event: Event,
    cancellation: &Cancellation,
    parked: &AtomicBool,
) -> Result<(), String> {
    loop {
        if cancellation.is_cancelled() {
            return Err("Preview cancelled".into());
        }
        match sender.try_send(event) {
            Ok(()) => return Ok(()),
            Err(mpsc::TrySendError::Disconnected(_)) => return Err("Preview closed".into()),
            Err(mpsc::TrySendError::Full(pending)) => event = pending,
        }
        thread::sleep(if parked.load(Ordering::Acquire) {
            PARKED_POLL
        } else {
            ACTIVE_POLL
        });
    }
}

fn render(
    source: &SandboxedMedia,
    start_tick: u32,
    cancellation: &Cancellation,
    sender: &mpsc::SyncSender<Event>,
    parked: &AtomicBool,
) -> Result<(), String> {
    let input = source
        .path
        .canonicalize()
        .map_err(|error| error.to_string())?;
    if !input.is_file() {
        return Err("Preview input is not a regular file".into());
    }
    let output = PrivateOutput::create().map_err(|error| error.to_string())?;
    let current = std::env::current_exe().map_err(|error| error.to_string())?;
    let running = PathBuf::from(format!("/proc/{}/exe", std::process::id()));
    let executable = resolve_renderer_executable(&current, &running, output.path())?;
    let bwrap = crate::trusted_command::resolve("bwrap")
        .map_err(|error| format!("Unable to start the preview sandbox: {error}"))?;
    let devices = gpu_devices(Path::new("/dev"), source.backend);
    let mut command = sandbox_command(
        &bwrap,
        &executable,
        &input,
        output.path(),
        ParseOperation::PreviewMedia(source.size),
        0,
        source.backend,
        &devices,
    );
    command
        .arg(start_tick.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if cancellation.is_cancelled() {
        return Err("Preview cancelled".into());
    }
    let mut child = spawn_renderer(&mut command)
        .map_err(|error| format!("Unable to start the preview sandbox: {error}"))?;
    let result = consume(&mut child, source, start_tick, cancellation, sender, parked);
    // Also tear down descendants which keep a pipe open or outlive their leader.
    if result.is_err() {
        terminate(&mut child);
    }
    result.map_err(|error| error.to_string())
}

fn consume(
    child: &mut Child,
    source: &SandboxedMedia,
    start_tick: u32,
    cancellation: &Cancellation,
    sender: &mpsc::SyncSender<Event>,
    parked: &AtomicBool,
) -> io::Result<()> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("Missing decoded-media pipe"))?;
    let mut reader = TimedReader {
        fd: &stdout,
        deadline: Instant::now() + STARTUP_TIMEOUT,
        cancellation,
    };
    let header = Header::read(&mut reader, source.size, start_tick)?;
    send(sender, Event::Prepared(header), cancellation, parked).map_err(io::Error::other)?;
    let mut decoder = Decoder::new(header);
    loop {
        reader.deadline = Instant::now() + FRAME_TIMEOUT;
        let packet = decoder.read(&mut reader)?;
        if let Packet::End(duration) = packet {
            reader.deadline = Instant::now() + Duration::from_secs(2);
            if reader.read(&mut [0])? != 0 {
                return Err(io::Error::other("Trailing decoded-media output"));
            }
            let status = wait_for_renderer(child, cancellation, Duration::from_secs(2))
                .map_err(io::Error::other)?;
            if !status.success() {
                return Err(io::Error::other("The sandboxed decoder failed"));
            }
            send(
                sender,
                Event::Packet(Packet::End(duration)),
                cancellation,
                parked,
            )
            .map_err(io::Error::other)?;
            return Ok(());
        }
        send(sender, Event::Packet(packet), cancellation, parked).map_err(io::Error::other)?;
    }
}

#[cfg(test)]
pub(crate) mod tests;
