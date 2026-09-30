// SPDX-License-Identifier: MIT

use std::rc::Rc;

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
