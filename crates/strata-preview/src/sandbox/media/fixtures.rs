// SPDX-License-Identifier: MIT

//! Streams that stand in for a sandboxed decoder in tests.

use super::*;
use crate::media::Frame;

pub fn stream(header: Header) -> Result<Session, String> {
    stream_to(header, header.duration_us)
}

pub fn stream_to(header: Header, end: u64) -> Result<Session, String> {
    let slot = WorkerSlot::acquire().ok_or("Media previews are busy (four active players)")?;
    Ok(synthetic(header, end, slot, Duration::ZERO))
}

pub fn stream_preload(header: Header) -> Result<Session, String> {
    stream_preload_after_header(header, Duration::ZERO)
}

/// A preload worker whose first frame follows its header after `hold`, as a slow decoder's would.
pub fn stream_preload_after_header(header: Header, hold: Duration) -> Result<Session, String> {
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
                samples: vec![0; crate::media::audio_bytes(header, tick)],
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

/// A session whose decoder dies before producing a header, as a seek past the
/// end of a truncated file does.
pub fn failing_stream() -> Result<Session, String> {
    let slot = WorkerSlot::acquire().ok_or("Media previews are busy (four active players)")?;
    let slot = Arc::new(slot);
    let worker_slot = slot.clone();
    let cancellation = Cancellation::default();
    let cancelled = cancellation.clone();
    let (sender, receiver) = mpsc::sync_channel(QUEUED_PACKETS);
    let worker = thread::spawn(move || {
        let _slot = worker_slot;
        let _sent = send(
            &sender,
            Event::Failed("failed to fill whole buffer".into()),
            &cancelled,
            &_slot.parked,
        );
    });
    Ok(Session {
        cancellation,
        receiver,
        worker,
        slot,
    })
}
