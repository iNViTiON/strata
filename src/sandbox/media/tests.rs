// SPDX-License-Identifier: MIT

use super::*;
use crate::media::Frame;

// The worker budgets are process-wide, so tests that count them cannot overlap.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    let guard = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // Slots return when cancelled worker threads exit, shortly after a test ends.
    wait_for(|| {
        ACTIVE_WORKERS.load(Ordering::Acquire) == 0 && PRELOAD_WORKERS.load(Ordering::Acquire) == 0
    });
    guard
}

pub(crate) fn stream(header: Header) -> Result<Session, String> {
    stream_to(header, header.duration_us)
}

pub(crate) fn stream_to(header: Header, end: u64) -> Result<Session, String> {
    let slot = WorkerSlot::acquire().ok_or("Media previews are busy (four active players)")?;
    Ok(synthetic(header, end, slot, Duration::ZERO))
}

pub(crate) fn stream_preload(header: Header) -> Result<Session, String> {
    stream_preload_after_header(header, Duration::ZERO)
}

/// A preload worker whose first frame follows its header after `hold`, as a slow decoder's would.
pub(crate) fn stream_preload_after_header(
    header: Header,
    hold: Duration,
) -> Result<Session, String> {
    let slot = WorkerSlot::acquire_preload().ok_or("Neighbor previews are using every worker")?;
    Ok(synthetic(header, header.duration_us, slot, hold))
}

fn synthetic(header: Header, end: u64, slot: WorkerSlot, hold: Duration) -> Session {
    let slot = Arc::new(slot);
    let worker_slot = slot.clone();
    let cancellation = Cancellation::default();
    let cancelled = cancellation.clone();
    let (sender, receiver) = mpsc::sync_channel(QUEUED_PACKETS);
    let worker = thread::spawn(move || {
        let slot = worker_slot;
        if send(&sender, Event::Prepared(header), &cancelled, &slot.parked).is_err() {
            return;
        }
        thread::sleep(hold);
        let end_tick = Header {
            duration_us: end,
            ..header
        }
        .ticks();
        for tick in header.start_tick..end_tick {
            let frame = Frame {
                tick,
                pixels: vec![(tick % 255) as u8; header.video_bytes()],
                samples: if header.audio {
                    vec![0; header.audio_bytes()]
                } else {
                    Vec::new()
                },
            };
            if send(
                &sender,
                Event::Packet(Packet::Frame(frame)),
                &cancelled,
                &slot.parked,
            )
            .is_err()
            {
                return;
            }
        }
        let _sent = send(
            &sender,
            Event::Packet(Packet::End(end)),
            &cancelled,
            &slot.parked,
        );
    });
    Session {
        cancellation,
        receiver,
        worker,
        slot,
    }
}

fn wait_for(condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !condition() {
        assert!(Instant::now() < deadline, "worker teardown deadline");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn four_slots_backpressure_cancellation_and_repeated_teardown_are_bounded() {
    let _serial = serial();
    let h = Header {
        width: 16,
        height: 16,
        audio: true,
        duration_us: 60_000_000,
        start_tick: 0,
        fps: 30,
        native_fps: 30,
    };
    for _ in 0..30 {
        let workers: Vec<_> = (0..MAX_WORKERS)
            .map(|_| stream(h).expect("available worker"))
            .collect();
        assert!(stream(h).is_err());
        thread::sleep(Duration::from_millis(20));
        for worker in &workers {
            assert!(
                !worker.finished(),
                "backpressure must stop whole-clip decoding"
            );
            worker.cancel();
        }
        wait_for(|| workers.iter().all(Session::finished));
        for worker in &workers {
            assert!(worker.receiver.try_iter().count() <= QUEUED_PACKETS);
            assert!(matches!(worker.receive(), Some(Event::Failed(_))));
        }
        drop(workers);
        assert_eq!(ACTIVE_WORKERS.load(Ordering::Acquire), 0);
    }
    let workers: Vec<_> = (0..MAX_WORKERS)
        .map(|_| {
            stream(Header {
                duration_us: 33_333,
                ..h
            })
            .expect("available worker")
        })
        .collect();
    wait_for(|| workers.iter().all(Session::finished));
    assert!(
        stream(h).is_err(),
        "buffered playback retains its slot between GIF loops"
    );
    drop(workers);
    assert_eq!(ACTIVE_WORKERS.load(Ordering::Acquire), 0);
}

#[test]
fn decoder_failure_and_trailing_output_are_not_successful_end_of_stream() {
    let source = SandboxedMedia {
        path: "/unused".into(),
        size: crate::services::MediaPreviewSize::new(16, 16),
        backend: MediaPreviewBackend::Software,
        input_owner: None,
    };
    let h = Header {
        width: 1,
        height: 1,
        audio: false,
        duration_us: 33_333,
        start_tick: 0,
        fps: 30,
        native_fps: 30,
    };
    let mut bytes = Vec::new();
    h.write(&mut bytes).expect("header");
    Frame {
        tick: 0,
        pixels: vec![0; 4],
        samples: vec![],
    }
    .write(&mut bytes, 30)
    .expect("frame");
    crate::media::write_end(&mut bytes, 1, h.duration_us).expect("end");
    let file = tempfile::NamedTempFile::new().expect("wire fixture");
    fs::write(file.path(), bytes).expect("wire bytes");
    for suffix in ["exit 1", "printf garbage"] {
        let mut child = spawn_renderer(
            Command::new("sh")
                .args(["-c", &format!("cat \"$1\"; {suffix}"), "fixture"])
                .arg(file.path())
                .stdout(Stdio::piped()),
        )
        .expect("worker");
        let (sender, receiver) = mpsc::sync_channel(8);
        assert!(
            consume(
                &mut child,
                &source,
                0,
                &Cancellation::default(),
                &sender,
                &AtomicBool::new(false)
            )
            .is_err()
        );
        assert!(
            !receiver
                .try_iter()
                .any(|event| matches!(event, Event::Packet(Packet::End(_))))
        );
        terminate(&mut child);
    }
}

fn long_header() -> Header {
    Header {
        width: 16,
        height: 16,
        audio: true,
        duration_us: 60_000_000,
        start_tick: 0,
        fps: 30,
        native_fps: 30,
    }
}

#[test]
fn preload_workers_have_their_own_bounded_budget() {
    let _serial = serial();
    let preloads: Vec<_> = (0..MAX_PRELOAD_WORKERS)
        .map(|_| stream_preload(long_header()).expect("preload worker"))
        .collect();
    assert!(stream_preload(long_header()).is_err());
    assert!(!Session::preload_available());
    let players: Vec<_> = (0..MAX_WORKERS)
        .map(|_| stream(long_header()).expect("interactive worker despite full preload budget"))
        .collect();
    assert!(stream(long_header()).is_err());
    assert_eq!(ACTIVE_WORKERS.load(Ordering::Acquire), MAX_WORKERS);
    assert_eq!(PRELOAD_WORKERS.load(Ordering::Acquire), MAX_PRELOAD_WORKERS);
    drop((preloads, players));
    wait_for(|| {
        ACTIVE_WORKERS.load(Ordering::Acquire) == 0 && PRELOAD_WORKERS.load(Ordering::Acquire) == 0
    });
    assert!(Session::preload_available());
}

#[test]
fn promotion_needs_a_free_interactive_slot_and_frees_the_preload_slot() {
    let _serial = serial();
    let parked = stream_preload(long_header()).expect("preload worker");
    let other = stream_preload(long_header()).expect("second preload worker");
    let mut players: Vec<_> = (0..MAX_WORKERS)
        .map(|_| stream(long_header()).expect("interactive worker"))
        .collect();
    assert!(!parked.promote(), "promotion must not exceed four players");
    assert_eq!(PRELOAD_WORKERS.load(Ordering::Acquire), 2);
    drop(players.pop());
    // The freed slot returns once the cancelled worker thread has exited.
    wait_for(|| parked.promote());
    assert!(parked.promote(), "promotion is idempotent");
    assert_eq!(PRELOAD_WORKERS.load(Ordering::Acquire), 1);
    assert_eq!(ACTIVE_WORKERS.load(Ordering::Acquire), MAX_WORKERS);
    drop((parked, other));
    wait_for(|| {
        PRELOAD_WORKERS.load(Ordering::Acquire) == 0
            && ACTIVE_WORKERS.load(Ordering::Acquire) == MAX_WORKERS - 1
    });
    drop(players);
}

#[test]
fn a_parked_worker_blocks_on_backpressure_and_stops_when_dropped() {
    let _serial = serial();
    let parked = stream_preload(long_header()).expect("preload worker");
    thread::sleep(Duration::from_millis(120));
    assert!(!parked.finished(), "parked decoding must stop at the queue");
    assert!(parked.receiver.try_iter().count() <= QUEUED_PACKETS);
    parked.cancel();
    wait_for(|| parked.finished());
    drop(parked);
    wait_for(|| PRELOAD_WORKERS.load(Ordering::Acquire) == 0);
}
