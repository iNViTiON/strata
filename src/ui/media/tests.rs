// SPDX-License-Identifier: MIT

use std::{cell::Cell, path::PathBuf, rc::Rc};

use super::*;
use crate::{sandbox::MediaPreviewBackend, sandbox::media::tests::stream};

const DURATION_US: u64 = 20_000_000;

fn source(width: i32, height: i32) -> SandboxedMedia {
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
    let media = DecodedMedia::new(source(320, 180));
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
