// SPDX-License-Identifier: MIT

//! Changing the decode size of a playing stream without a visible restart.
//!
//! A second decoder starts at the new size a little ahead of the playhead and
//! decodes in the background while the first keeps playing. Once the second
//! has reached the moment right after the last frame the first delivered, the
//! stream switches over: earlier frames and their audio came from the first
//! decoder, later ones from the second, and the audio output and playback clock
//! carry on unchanged. The two may run at different frame rates; both start
//! and switch on whole ticks of the 30 fps seek grid, where their ticks meet.

use std::time::{Duration, Instant};

use super::*;

const TIMEOUT: Duration = Duration::from_secs(4);
const DEFAULT_LATENCY: Duration = Duration::from_millis(700);
const MIN_LEAD: Duration = Duration::from_millis(250);
const MAX_LEAD: Duration = Duration::from_millis(1500);
const LEAD_MARGIN: f64 = 1.2;
/// A handover this close to the end is not worth a second decoder.
const MIN_TICKS_LEFT: u32 = 8;

pub(super) struct Handover {
    session: Session,
    size: MediaPreviewSize,
    header: Option<Header>,
    /// A frame from the new decoder waiting for the old one to catch up.
    pending: Option<Frame>,
    started: Instant,
}

impl DecodedMedia {
    /// Whether the stream can change size while it keeps playing.
    fn can_hand_over(&self) -> bool {
        let imp = self.imp();
        self.is_playing()
            && imp.first_frame.get()
            && imp.end.get().is_none()
            && !imp.dormant.get()
            && imp.restart.get().is_none()
            && imp.session.borrow().is_some()
            && imp.header.get().is_some_and(|header| header.width > 0)
    }

    /// Starts decoding at the new size in the background. `false` means the
    /// stream is not in a state for it and should restart the plain way.
    pub(super) fn begin_handover(&self) -> bool {
        let imp = self.imp();
        let Some(source) = imp.source.borrow().clone() else {
            return false;
        };
        if imp
            .handover
            .borrow()
            .as_ref()
            .is_some_and(|handover| handover.size == source.size)
        {
            return true;
        }
        if !self.can_hand_over() {
            return false;
        }
        imp.handover.borrow_mut().take();
        let Some(header) = imp.header.get() else {
            return false;
        };
        self.capture_position();
        let lead = imp
            .start_latency
            .get()
            .unwrap_or(DEFAULT_LATENCY)
            .mul_f64(LEAD_MARGIN)
            .clamp(MIN_LEAD, MAX_LEAD);
        let playhead = media::seek_tick(imp.position.get(), header.duration_us);
        let start = playhead + (lead.as_secs_f64() * f64::from(media::FPS)).round() as u32;
        if start + MIN_TICKS_LEFT >= header.seek_ticks() {
            // Too close to the end to matter; the stream keeps its current size.
            return true;
        }
        let size = source.size;
        #[cfg(test)]
        let started = match imp.loader.borrow().as_ref() {
            Some(loader) => loader(source, start),
            None => Session::start(source, start),
        };
        #[cfg(not(test))]
        let started = Session::start(source, start);
        match started {
            Ok(session) => {
                imp.handover.replace(Some(Handover {
                    session,
                    size,
                    header: None,
                    pending: None,
                    started: Instant::now(),
                }));
            }
            Err(error) => {
                // No free worker: the current decode carries on at its old size
                // and the switch is tried again once one may have freed up.
                tracing::debug!(error, "media handover could not start");
                imp.resized.set(Some(Instant::now()));
            }
        }
        true
    }

    /// Feeds the background decoder and switches over when it lines up.
    pub(super) fn pump_handover(&self) -> Result<(), String> {
        let imp = self.imp();
        let Some((handover, seam)) = self.next_switch()? else {
            return Ok(());
        };
        let Some(header) = handover.header else {
            return Ok(());
        };
        imp.session.replace(Some(handover.session));
        imp.header.set(Some(header));
        imp.loaded_size.set(Some(handover.size));
        // The seam frame is the second decoder's first, with the start of its
        // audio eased in from the first decoder's.
        self.accept_frame(seam)?;
        tracing::debug!(
            width = header.width,
            height = header.height,
            "sandboxed media handed over to a new decoder"
        );
        Ok(())
    }

    /// The finished handover and the second decoder's first frame, once both
    /// decoders are at the same moment.
    fn next_switch(&self) -> Result<Option<(Handover, Frame)>, String> {
        let imp = self.imp();
        let mut slot = imp.handover.borrow_mut();
        let Some(handover) = slot.as_mut() else {
            return Ok(None);
        };
        let abandoned = handover.started.elapsed() > TIMEOUT
            || imp.end.get().is_some()
            || imp.dormant.get()
            || imp.restart.get().is_some();
        if abandoned {
            slot.take();
            return Ok(None);
        }
        let (Some(last), Some(current)) = (imp.last_tick.get(), imp.header.get()) else {
            return Ok(None);
        };
        let mut ready = None;
        loop {
            if let Some(frame) = handover.pending.take() {
                let Some(next) = handover.header else {
                    handover.pending = Some(frame);
                    break;
                };
                // Two ticks are the same moment when they are equal in whole
                // ticks of the seek grid: scale each by the other decoder's rate.
                let incoming = u64::from(frame.tick) * u64::from(current.fps / media::FPS);
                let outgoing = u64::from(last + 1) * u64::from(next.fps / media::FPS);
                match incoming.cmp(&outgoing) {
                    std::cmp::Ordering::Less => continue,
                    std::cmp::Ordering::Equal => {
                        ready = Some(frame);
                        break;
                    }
                    std::cmp::Ordering::Greater => {
                        handover.pending = Some(frame);
                        break;
                    }
                }
            }
            match handover.session.receive() {
                None => break,
                Some(Event::Prepared(header)) => handover.header = Some(header),
                Some(Event::Packet(Packet::Frame(frame))) => handover.pending = Some(frame),
                Some(Event::Packet(Packet::End(_))) | Some(Event::Failed(_)) => {
                    slot.take();
                    return Ok(None);
                }
            }
        }
        let Some(incoming) = ready else {
            return Ok(None);
        };
        // The first decoder has the next tick queued; wait a frame if it does not yet.
        let outgoing = imp.session.borrow().as_ref().and_then(Session::receive);
        let outgoing = match outgoing {
            None => {
                handover.pending = Some(incoming);
                return Ok(None);
            }
            Some(Event::Packet(Packet::Frame(frame))) if frame.tick == last + 1 => frame,
            Some(Event::Failed(error)) => return Err(error),
            Some(_) => return Err("Unexpected media event during a handover".to_owned()),
        };
        let mut seam = incoming;
        if !outgoing.samples.is_empty() && !seam.samples.is_empty() {
            seam.samples = crossfade(&outgoing.samples, &seam.samples);
        }
        Ok(slot.take().map(|handover| (handover, seam)))
    }
}

/// 16-bit stereo audio at 48 kHz, eased from `outgoing` to `incoming` over the
/// first 10 ms. Decoders that start from a seek can land a few samples apart
/// (Opus does), which a straight join turns into a click. The chunks may differ
/// in length when the decoders run at different frame rates.
fn crossfade(outgoing: &[u8], incoming: &[u8]) -> Vec<u8> {
    const BYTES_PER_FRAME: usize = 4;
    const FADE_FRAMES: usize = (media::SAMPLE_RATE as usize) / 100;
    let mut mixed = incoming.to_vec();
    let fade = FADE_FRAMES
        .min(mixed.len() / BYTES_PER_FRAME)
        .min(outgoing.len() / BYTES_PER_FRAME);
    for frame in 0..fade {
        let weight = frame as f32 / fade as f32;
        for byte in (frame * BYTES_PER_FRAME..(frame + 1) * BYTES_PER_FRAME).step_by(2) {
            let sample = |data: &[u8]| f32::from(i16::from_le_bytes([data[byte], data[byte + 1]]));
            let value = sample(outgoing) * (1.0 - weight) + sample(incoming) * weight;
            mixed[byte..byte + 2].copy_from_slice(&(value.round() as i16).to_le_bytes());
        }
    }
    mixed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(frames: usize, level: i16) -> Vec<u8> {
        std::iter::repeat_n(level.to_le_bytes(), frames * 2)
            .flatten()
            .collect()
    }

    fn left(data: &[u8], frame: usize) -> i16 {
        i16::from_le_bytes([data[frame * 4], data[frame * 4 + 1]])
    }

    #[test]
    fn a_crossfade_eases_from_the_old_audio_into_the_new_within_ten_milliseconds() {
        let old = tone(1_600, 1_000);
        let new = tone(1_600, -1_000);
        let mixed = crossfade(&old, &new);
        assert_eq!(mixed.len(), new.len());
        assert_eq!(left(&mixed, 0), 1_000, "starts where the old audio was");
        assert!((0..479).all(|frame| left(&mixed, frame) >= left(&mixed, frame + 1)));
        assert_eq!(left(&mixed, 480), -1_000, "10 ms in, only the new audio");
        assert_eq!(&mixed[480 * 4..], &new[480 * 4..]);
        let step = |frame: usize| i32::from(left(&mixed, frame + 1) - left(&mixed, frame)).abs();
        assert!(
            (0..479).all(|frame| step(frame) <= 5),
            "no jump larger than 5"
        );
    }

    #[test]
    fn identical_audio_passes_through_a_crossfade_unchanged() {
        let audio: Vec<u8> = (0..1_600 * 2)
            .flat_map(|sample: i32| ((sample % 300 - 150) * 47).to_le_bytes()[..2].to_vec())
            .collect();
        assert_eq!(crossfade(&audio, &audio), audio);
    }

    #[test]
    fn a_short_frame_is_faded_over_what_there_is() {
        let mixed = crossfade(&tone(100, 500), &tone(100, 900));
        assert_eq!(mixed.len(), 100 * 4);
        assert_eq!(left(&mixed, 0), 500);
        assert!(left(&mixed, 99) > 800);
    }
}
