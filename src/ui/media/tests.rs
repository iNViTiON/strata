// SPDX-License-Identifier: MIT

use std::{cell::Cell, path::PathBuf, rc::Rc, time::Duration};

use super::*;
use crate::sandbox::{MediaPreviewBackend, media::tests::stream};

const TEST_DURATION_US: u64 = 60_000_000;

fn test_header(start_tick: u32) -> Header {
    Header {
        width: 16,
        height: 16,
        audio: false,
        duration_us: TEST_DURATION_US,
        start_tick,
        fps: 30,
        native_fps: 30,
    }
}

fn test_source(path: &str) -> SandboxedMedia {
    SandboxedMedia {
        path: PathBuf::from(path),
        size: MediaPreviewSize::new(320, 240),
        backend: crate::sandbox::MediaPreviewBackend::Software,
        input_owner: None,
    }
}

fn fake_loader(calls: &Rc<RefCell<Vec<u32>>>) -> TestLoader {
    let calls = calls.clone();
    Rc::new(move |_source: SandboxedMedia, tick: u32| {
        calls.borrow_mut().push(tick);
        crate::sandbox::media::tests::stream(test_header(tick))
    })
}

fn drive_until(media: &DecodedMedia, calls: &Rc<RefCell<Vec<u32>>>, want: usize) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while calls.borrow().len() < want {
        match media.tick() {
            Ok(()) => {}
            // Worker slots are shared with other tests.
            Err(error) if error.contains("busy") => {}
            Err(error) => panic!("tick failed: {error}"),
        }
        assert!(Instant::now() < deadline, "decode worker deadline");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn open_restoring(path: &str, position: u64) -> (DecodedMedia, Rc<RefCell<Vec<u32>>>) {
    remember_media_position(path.into(), position);
    let media = DecodedMedia::new(test_source(path));
    let calls = Rc::new(RefCell::new(Vec::new()));
    media.imp().loader.replace(Some(fake_loader(&calls)));
    media.upcast_ref::<gtk::MediaStream>().play();
    (media, calls)
}

#[test]
fn reopening_resumes_where_the_preview_closed() {
    gtk::init().expect("GTK display");

    let (media, calls) = open_restoring("/remembered", media::timestamp(900));
    let deadline = Instant::now() + Duration::from_secs(15);
    while (media.timestamp() as u64) < media::timestamp(900) {
        media.tick().expect("tick succeeds");
        assert!(Instant::now() < deadline, "resume deadline");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(*calls.borrow(), vec![0, 900]);
    let position = media.timestamp() as u64;
    media.close();
    assert_eq!(
        recall_media_position(Path::new("/remembered")),
        Some(position)
    );

    let (media, calls) = open_restoring("/shortened", TEST_DURATION_US + 1);
    drive_until(&media, &calls, 1);
    for _ in 0..20 {
        media.tick().expect("tick succeeds");
    }
    assert_eq!(*calls.borrow(), vec![0]);
    assert!(media.imp().first_frame.get());
    media.close();
    assert_eq!(recall_media_position(Path::new("/shortened")), None);

    let (media, calls) = open_restoring("/ended", media::timestamp(900));
    drive_until(&media, &calls, 2);
    for _ in 0..20 {
        media.tick().expect("tick succeeds");
    }
    media.imp().position.set(TEST_DURATION_US);
    media.close();
    assert_eq!(recall_media_position(Path::new("/ended")), None);

    let (media, calls) = open_restoring("/ended", media::timestamp(900));
    drive_until(&media, &calls, 2);
    media.imp().position.set(RESTORE_MIN_US);
    media.close();
    assert_eq!(recall_media_position(Path::new("/ended")), None);
}

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
/// and later decoders start, and starts numbered in `busy` fail.
fn loader(
    loads: &Rc<Cell<usize>>,
    offset: u32,
    busy: Option<std::ops::Range<usize>>,
) -> TestLoader {
    let loads = loads.clone();
    Rc::new(move |source: SandboxedMedia, tick| {
        let count = loads.get();
        loads.set(count + 1);
        if busy.as_ref().is_some_and(|busy| busy.contains(&count)) {
            return Err("Media previews are busy".to_owned());
        }
        let (width, height) = (source.size.width as u32, source.size.height as u32);
        let fps = if source.size.expanded {
            media::frame_rate_for(width, height, 60, source.size.max_fps)
        } else {
            media::FPS
        };
        stream(Header {
            width,
            height,
            audio: false,
            duration_us: DURATION_US,
            start_tick: (if count == 0 { tick } else { tick + offset }) * (fps / media::FPS),
            fps,
            native_fps: 60,
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

fn playing(
    loads: &Rc<Cell<usize>>,
    offset: u32,
    busy: Option<std::ops::Range<usize>>,
) -> DecodedMedia {
    let media = DecodedMedia::new(source(320, 180));
    media
        .imp()
        .loader
        .replace(Some(loader(loads, offset, busy)));
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
            assert_eq!(imp.origin_us.get(), 0, "the timeline keeps its origin");
            let handed_over_at = imp.header.get().expect("header").start_tick;
            assert!(
                handed_over_at > 0,
                "the second decoder started ahead of the playhead"
            );
            assert_eq!(
                media.audio_timestamp(handed_over_at + 3, 30),
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
fn a_faster_decoder_takes_over_on_the_seek_grid_and_hands_back() {
    crate::test_support::gtk_test(
        "ui::media::tests::a_faster_decoder_takes_over_on_the_seek_grid_and_hands_back",
        || {
            let loads = Rc::new(Cell::new(0));
            let media = playing(&loads, 0, None);
            assert_eq!(media.imp().header.get().expect("header").fps, 30);

            media.resize(MediaPreviewSize::expanded(640, 360));
            let mut previous = media.timestamp();
            spin_until("handover to 60 fps", || {
                let now = media.timestamp();
                assert!(now >= previous, "the playhead never moves back");
                previous = now;
                media
                    .imp()
                    .header
                    .get()
                    .is_some_and(|header| header.fps == 60)
                    && picture_size(&media) == (640, 360)
            });
            let header = media.imp().header.get().expect("header");
            assert_eq!(
                loads.get(),
                2,
                "one decoder to start with, one to hand over to"
            );
            assert!(header.start_tick > 0 && header.start_tick.is_multiple_of(2));
            assert_eq!(
                media.imp().origin_us.get(),
                0,
                "the timeline keeps its origin"
            );
            assert_eq!(
                media.audio_timestamp(header.start_tick + 3, 60),
                media::timestamp_at(header.start_tick + 3, 60),
                "audio after the switch continues the same timeline"
            );
            assert!(media.is_playing());
            assert!(media.error().is_none());

            let after = media.timestamp();
            spin_until("playback continues", || media.timestamp() > after + 100_000);
            media.resize(MediaPreviewSize::new(320, 180));
            spin_until("handover back to 30 fps", || {
                media
                    .imp()
                    .header
                    .get()
                    .is_some_and(|header| header.fps == 30)
                    && picture_size(&media) == (320, 180)
            });
            assert_eq!(loads.get(), 3);
            assert!(media.is_playing());
            assert!(media.error().is_none());
        },
    );
}

#[test]
fn a_slow_screen_keeps_the_expanded_view_at_thirty_frames() {
    crate::test_support::gtk_test(
        "ui::media::tests::a_slow_screen_keeps_the_expanded_view_at_thirty_frames",
        || {
            let loads = Rc::new(Cell::new(0));
            let media = playing(&loads, 0, None);
            media.resize(MediaPreviewSize::expanded(640, 360).for_refresh_rate(30_000));
            spin_until("handover", || picture_size(&media) == (640, 360));
            assert_eq!(media.imp().header.get().expect("header").fps, 30);
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
fn a_handover_that_finds_no_free_worker_is_retried_while_the_stream_keeps_playing() {
    crate::test_support::gtk_test(
        "ui::media::tests::a_handover_that_finds_no_free_worker_is_retried_while_the_stream_keeps_playing",
        || {
            let loads = Rc::new(Cell::new(0));
            let media = playing(&loads, 0, Some(1..3));
            media.resize(MediaPreviewSize::new(640, 360));
            spin_until("the first attempt fails", || loads.get() == 2);
            let before = media.timestamp();
            spin_until("playback goes on", || media.timestamp() > before + 100_000);
            assert_eq!(
                picture_size(&media),
                (320, 180),
                "the old size stays for now"
            );
            assert!(media.is_playing());
            assert!(media.error().is_none());

            spin_until("the retry hands over", || {
                picture_size(&media) == (640, 360)
            });
            assert_eq!(loads.get(), 4, "two busy attempts, then one that worked");
            assert!(media.is_playing());
            assert!(media.error().is_none());
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
