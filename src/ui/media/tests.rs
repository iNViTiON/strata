// SPDX-License-Identifier: MIT

use std::{cell::Cell, path::PathBuf, rc::Rc};

use super::*;
use crate::{
    sandbox::{
        MediaPreviewBackend,
        media::{
            MAX_WORKERS,
            tests::{stream, stream_preload, stream_preload_after_header},
        },
    },
    test_support::gtk_test,
};

fn header(source: &SandboxedMedia, audio: bool, start_tick: u32) -> Header {
    Header {
        width: source.size.width as u32,
        height: source.size.height as u32,
        audio,
        duration_us: 3_600_000_000,
        start_tick,
    }
}

fn source() -> SandboxedMedia {
    SandboxedMedia {
        path: "/unused".into(),
        size: MediaPreviewSize::new(160, 90),
        backend: MediaPreviewBackend::Software,
        input_owner: None,
    }
}

/// A parked stream whose "sandbox" is the synthetic decoder; `loads` counts worker starts.
fn parked(audio: bool, loads: &Rc<Cell<u32>>) -> DecodedMedia {
    let player = DecodedMedia::preload(source());
    let loads = loads.clone();
    player.use_test_loader(Rc::new(move |source, start_tick| {
        loads.set(loads.get() + 1);
        stream_preload(header(&source, audio, start_tick))
    }));
    player
}

fn wait(condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "playback deadline");
        glib::MainContext::default().iteration(false);
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn settle(duration: Duration) {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        glib::MainContext::default().iteration(false);
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn a_parked_stream_shows_its_first_frame_silently_and_goes_idle() {
    gtk_test(
        "ui::media::tests::a_parked_stream_shows_its_first_frame_silently_and_goes_idle",
        || {
            let loads = Rc::new(Cell::new(0));
            let player = parked(true, &loads);
            wait(|| player.is_parked_ready());
            wait(|| player.imp().timer.borrow().is_none());
            assert!(player.imp().texture.borrow().is_some());
            assert!(
                player.imp().audio.borrow().is_none(),
                "no audio stream for a neighbor"
            );
            player.play();
            assert!(!player.is_playing(), "a parked stream is not a player yet");
            settle(Duration::from_millis(150));
            assert!(
                player
                    .imp()
                    .session
                    .borrow()
                    .as_ref()
                    .is_some_and(|session| !session.finished()),
                "backpressure keeps the worker alive but idle"
            );
            assert_eq!(loads.get(), 1);
            player.close();
        },
    );
}

#[test]
fn promotion_resumes_the_same_worker_with_audio_and_never_restarts_it() {
    gtk_test(
        "ui::media::tests::promotion_resumes_the_same_worker_with_audio_and_never_restarts_it",
        || {
            let loads = Rc::new(Cell::new(0));
            let player = parked(true, &loads);
            wait(|| player.is_parked_ready());
            assert!(player.promote());
            assert!(
                player.imp().audio.borrow().is_some(),
                "promotion opens audio"
            );
            player.set_muted(true);
            player.play();
            assert!(player.is_playing());
            wait(|| {
                assert!(player.error().is_none(), "{:?}", player.error());
                player.timestamp() > 100_000
            });
            // The pane reports the size it decoded at: no restart, ever.
            player.resize(source().size);
            settle(Duration::from_millis(400));
            assert_eq!(loads.get(), 1, "promotion and resize keep the worker");
            assert!(player.imp().restart.get().is_none());
            assert!(player.error().is_none());
            player.close();
        },
    );
}

#[test]
fn promotion_waits_for_an_interactive_slot() {
    gtk_test(
        "ui::media::tests::promotion_waits_for_an_interactive_slot",
        || {
            let loads = Rc::new(Cell::new(0));
            let player = parked(false, &loads);
            wait(|| player.is_parked_ready());
            let header = header(&source(), false, 0);
            let mut players: Vec<_> = (0..MAX_WORKERS)
                .map(|_| stream(header).expect("interactive worker"))
                .collect();
            assert!(!player.promote(), "four players are already running");
            assert!(player.imp().parked.get());
            drop(players.pop());
            // The freed slot returns once the cancelled worker thread has exited.
            wait(|| player.promote());
            assert!(!player.imp().parked.get());
            player.close();
        },
    );
}

#[test]
fn promotion_between_the_header_and_the_first_frame_still_opens_audio() {
    gtk_test(
        "ui::media::tests::promotion_between_the_header_and_the_first_frame_still_opens_audio",
        || {
            let player = DecodedMedia::preload(source());
            player.use_test_loader(Rc::new(|source, start_tick| {
                stream_preload_after_header(
                    header(&source, true, start_tick),
                    Duration::from_millis(300),
                )
            }));
            wait(|| player.imp().header.get().is_some());
            assert!(
                !player.imp().first_frame.get(),
                "the decoder is still working"
            );
            assert!(player.imp().audio.borrow().is_none());
            assert!(player.promote(), "a stream still starting can be promoted");
            assert!(
                player.imp().audio.borrow().is_some(),
                "the audio output must not be lost between header and first frame"
            );
            player.set_muted(true);
            player.play();
            wait(|| {
                assert!(player.error().is_none(), "{:?}", player.error());
                player.timestamp() > 100_000
            });
            player.close();
        },
    );
}

const DURATION_US: u64 = 20_000_000;

fn sized_source(width: i32, height: i32) -> SandboxedMedia {
    SandboxedMedia {
        path: PathBuf::from("/nonexistent/clip.mp4"),
        size: MediaPreviewSize::new(width, height),
        backend: MediaPreviewBackend::Software,
        input_owner: None,
    }
}

/// Fake decoders sized like the request. `offset` shifts where the second
/// and later decoders start, and `busy_after` makes later starts fail.
fn loader(loads: &Rc<Cell<usize>>, offset: u32, busy_after: Option<usize>) -> TestLoader {
    let loads = loads.clone();
    Rc::new(move |source: SandboxedMedia, tick| {
        let count = loads.get();
        loads.set(count + 1);
        if busy_after.is_some_and(|limit| count >= limit) {
            return Err("Media previews are busy".to_owned());
        }
        stream(Header {
            width: source.size.width as u32,
            height: source.size.height as u32,
            audio: false,
            duration_us: DURATION_US,
            start_tick: if count == 0 { tick } else { tick + offset },
        })
    })
}

fn spin_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !condition() {
        assert!(Instant::now() < deadline, "{what}");
        glib::MainContext::default().iteration(false);
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn spin_for(milliseconds: u64) {
    let end = Instant::now() + Duration::from_millis(milliseconds);
    while Instant::now() < end {
        glib::MainContext::default().iteration(false);
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn picture_size(media: &DecodedMedia) -> (i32, i32) {
    (media.intrinsic_width(), media.intrinsic_height())
}

fn playing(loads: &Rc<Cell<usize>>, offset: u32, busy_after: Option<usize>) -> DecodedMedia {
    let media = DecodedMedia::new(sized_source(320, 180));
    media
        .imp()
        .loader
        .replace(Some(loader(loads, offset, busy_after)));
    media.play();
    spin_until("first frames", || {
        picture_size(&media) == (320, 180) && media.timestamp() > 300_000
    });
    media
}

#[test]
fn a_playing_stream_changes_size_without_stopping_or_going_back() {
    crate::test_support::gtk_test(
        "ui::media::tests::a_playing_stream_changes_size_without_stopping_or_going_back",
        || {
            let loads = Rc::new(Cell::new(0));
            let media = playing(&loads, 0, None);
            assert_eq!(loads.get(), 1);

            media.resize(MediaPreviewSize::new(640, 360));
            let mut previous = media.timestamp();
            spin_until("handover to the larger decoder", || {
                let now = media.timestamp();
                assert!(
                    now >= previous,
                    "the playhead never moves back: {previous} -> {now}"
                );
                previous = now;
                picture_size(&media) == (640, 360)
            });

            assert_eq!(
                loads.get(),
                2,
                "one decoder to start with, one for the handover"
            );
            assert!(media.is_playing());
            assert!(media.error().is_none());
            assert!(!media.is_ended());
            let imp = media.imp();
            assert!(imp.handover.borrow().is_none());
            assert_eq!(imp.origin_tick.get(), 0, "the timeline keeps its origin");
            let handed_over_at = imp.header.get().expect("header").start_tick;
            assert!(
                handed_over_at > 0,
                "the second decoder started ahead of the playhead"
            );
            assert_eq!(
                media.audio_timestamp(handed_over_at + 3),
                media::timestamp(handed_over_at + 3),
                "audio after the switch continues the same timeline"
            );

            let after = media.timestamp();
            spin_until("playback continues", || media.timestamp() > after + 100_000);
            assert_eq!(loads.get(), 2, "no restart");
        },
    );
}

#[test]
fn shrinking_hands_over_too() {
    crate::test_support::gtk_test("ui::media::tests::shrinking_hands_over_too", || {
        let loads = Rc::new(Cell::new(0));
        let media = playing(&loads, 0, None);
        media.resize(MediaPreviewSize::new(640, 360));
        spin_until("larger", || picture_size(&media) == (640, 360));
        media.resize(MediaPreviewSize::new(320, 180));
        spin_until("smaller again", || picture_size(&media) == (320, 180));
        assert_eq!(loads.get(), 3);
        assert!(media.is_playing());
        assert!(media.error().is_none());
    });
}

#[test]
fn a_paused_stream_restarts_at_its_position_instead() {
    crate::test_support::gtk_test(
        "ui::media::tests::a_paused_stream_restarts_at_its_position_instead",
        || {
            let loads = Rc::new(Cell::new(0));
            let media = playing(&loads, 0, None);
            media.pause();
            let position = media.timestamp();
            media.resize(MediaPreviewSize::new(640, 360));
            spin_until("restarted at the new size", || {
                picture_size(&media) == (640, 360)
            });
            assert_eq!(loads.get(), 2);
            assert!(!media.is_playing());
            assert!(media.imp().handover.borrow().is_none());
            assert!(
                media.timestamp().abs_diff(position) < 200_000,
                "still at {position}, now {}",
                media.timestamp()
            );
        },
    );
}

#[test]
fn a_busy_worker_pool_leaves_the_current_decode_playing() {
    crate::test_support::gtk_test(
        "ui::media::tests::a_busy_worker_pool_leaves_the_current_decode_playing",
        || {
            let loads = Rc::new(Cell::new(0));
            let media = playing(&loads, 0, Some(1));
            media.resize(MediaPreviewSize::new(640, 360));
            spin_until("the handover was attempted", || loads.get() == 2);
            let before = media.timestamp();
            spin_until("playback goes on", || media.timestamp() > before + 300_000);
            assert_eq!(picture_size(&media), (320, 180), "the old size stays");
            assert!(media.is_playing());
            assert!(media.error().is_none());
            assert_eq!(loads.get(), 2, "it does not retry in a loop");
        },
    );
}

#[test]
fn seeking_and_a_decoder_that_ends_early_both_drop_the_handover() {
    crate::test_support::gtk_test(
        "ui::media::tests::seeking_and_a_decoder_that_ends_early_both_drop_the_handover",
        || {
            let loads = Rc::new(Cell::new(0));
            let media = playing(&loads, 200, None);
            media.resize(MediaPreviewSize::new(640, 360));
            spin_until("handover pending", || {
                media.imp().handover.borrow().is_some()
            });
            media.seek(5_000_000);
            assert!(
                media.imp().handover.borrow().is_none(),
                "seeking cancels it"
            );

            let loads = Rc::new(Cell::new(0));
            let media = playing(&loads, 5_000, None);
            media.resize(MediaPreviewSize::new(640, 360));
            spin_until("handover pending", || loads.get() == 2);
            spin_for(300);
            assert!(
                media.imp().handover.borrow().is_none(),
                "a decoder past the end is dropped"
            );
            assert_eq!(picture_size(&media), (320, 180));
            assert!(media.is_playing());
            assert!(media.error().is_none());
        },
    );
}
