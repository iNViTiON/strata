// SPDX-License-Identifier: MIT

use std::{path::PathBuf, rc::Rc, time::Duration};

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

const TEST_DURATION_US: u64 = 60_000_000;

fn test_header(start_tick: u32) -> Header {
    Header {
        width: 16,
        height: 16,
        audio: false,
        duration_us: TEST_DURATION_US,
        start_tick,
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

#[test]
fn discarding_a_parked_neighbor_keeps_the_saved_position() {
    gtk_test(
        "ui::media::tests::discarding_a_parked_neighbor_keeps_the_saved_position",
        || {
            let path = "/neighbor-with-position";
            remember_media_position(path.into(), media::timestamp(900));
            let loads = Rc::new(Cell::new(0));
            let player = parked(false, &loads);
            player.imp().source.borrow_mut().as_mut().unwrap().path = path.into();
            player.close();
            assert_eq!(
                recall_media_position(Path::new(path)),
                Some(media::timestamp(900))
            );
        },
    );
}
