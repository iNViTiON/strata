// SPDX-License-Identifier: MIT

use std::{cell::Cell, path::PathBuf, rc::Rc, time::Duration};

use super::*;
use gstreamer as gst;
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
        audio_only: false,
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

    remember_media_position("/movie.ogg".into(), media::timestamp(900));
    let media = DecodedMedia::new(SandboxedMedia {
        audio_only: true,
        ..test_source("/movie.ogg")
    });
    let calls = Rc::new(RefCell::new(Vec::new()));
    media.imp().loader.replace(Some(fake_loader(&calls)));
    drive_until(&media, &calls, 2);
    assert_eq!(
        *calls.borrow(),
        vec![0, 900],
        "probed video resumes despite an audio extension"
    );
    media.close();

    remember_media_position("/song".into(), media::timestamp(900));
    let media = DecodedMedia::new(SandboxedMedia {
        audio_only: true,
        ..test_source("/song")
    });
    let calls = Rc::new(RefCell::new(Vec::new()));
    let audio_calls = calls.clone();
    media.imp().loader.replace(Some(Rc::new(move |_, tick| {
        audio_calls.borrow_mut().push(tick);
        crate::sandbox::media::tests::stream(Header {
            width: 0,
            height: 0,
            audio: true,
            ..test_header(tick)
        })
    })));
    media.upcast_ref::<gtk::MediaStream>().play();
    drive_until(&media, &calls, 1);
    for _ in 0..20 {
        media.tick().expect("tick succeeds");
    }
    assert_eq!(*calls.borrow(), vec![0]);
    media.imp().position.set(media::timestamp(600));
    media.close();
    assert_eq!(recall_media_position(Path::new("/song")), None);
}

fn pcm(values: impl IntoIterator<Item = i16>) -> Vec<u8> {
    values
        .into_iter()
        .flat_map(|value| {
            let bytes = value.to_le_bytes();
            [bytes[0], bytes[1], bytes[0], bytes[1]]
        })
        .collect()
}

#[test]
fn played_audio_window_ends_at_the_playhead_not_the_decoder() {
    let mut history = PcmHistory::default();
    history.push(&pcm((0..8).map(|value| value * 4096)));
    let mut window = [1.0; 4];

    assert!(history.window_ending_at(6, &mut window));
    assert_eq!(window, [0.25, 0.375, 0.5, 0.625]);

    assert!(history.window_ending_at(2, &mut window));
    assert_eq!(window, [0.0, 0.0, 0.0, 0.125]);

    assert!(history.window_ending_at(100, &mut window));
    assert_eq!(
        window[3], 0.875,
        "a lagging sink never reads past decoded audio"
    );
}

#[test]
fn played_audio_history_keeps_unplayed_samples_and_drops_old_ones() {
    let mut history = PcmHistory::default();
    history.push(&pcm(std::iter::repeat_n(1, 5 * HISTORY_SAMPLES)));
    let mut window = [0.0; 4];
    let window_samples = HISTORY_SAMPLES as u64;

    history.trim_behind(window_samples / 2);
    assert!(
        history.window_ending_at(window_samples / 2, &mut window),
        "a sink far behind the decoder still finds its samples"
    );
    assert!(history.window_ending_at(5, &mut window));

    history.trim_behind(3 * window_samples);
    assert!(!history.window_ending_at(5, &mut window));
    assert!(!history.window_ending_at(2 * window_samples - 1, &mut window));
    assert!(history.window_ending_at(2 * window_samples + 4, &mut window));
    assert!(history.window_ending_at(5 * window_samples, &mut window));

    history.clear();
    assert!(!history.window_ending_at(1, &mut window));
}

// A sink whose position follows rendered data, like PipeWire-Pulse and
// Bluetooth sinks: it holds `hold_ms` of PCM before rendering anything and then
// takes `block_us` per 33-ms block.
fn model_sink(hold_ms: u64, block_us: u32) -> String {
    format!(
        "queue min-threshold-time={} max-size-time={} max-size-bytes=0 max-size-buffers=0 \
         ! identity name=consumer sleep-time={block_us} ! fakesink sync=false",
        hold_ms * 1_000_000,
        (hold_ms + 200) * 1_000_000
    )
}

fn open_with(
    header: Header,
    sink: String,
    clock: Option<gst::Clock>,
    retain_audio: bool,
) -> DecodedMedia {
    let media = DecodedMedia::new(SandboxedMedia {
        audio_only: header.width == 0,
        ..test_source("/model-sink")
    });
    if retain_audio {
        media.retain_played_audio();
    }
    media.imp().audio_sink.replace(Some(sink));
    media.imp().audio_clock.replace(clock);
    let calls = Rc::new(RefCell::new(Vec::new()));
    let loader_calls = calls.clone();
    media.imp().loader.replace(Some(Rc::new(move |_, tick| {
        loader_calls.borrow_mut().push(tick);
        crate::sandbox::media::tests::stream(Header {
            start_tick: tick,
            ..header
        })
    })));
    media.upcast_ref::<gtk::MediaStream>().play();
    drive_until(&media, &calls, 1);
    // The first tick also initialises GStreamer, which must not land in a timed window.
    let deadline = Instant::now() + Duration::from_secs(15);
    while !media.imp().first_frame.get() {
        media.tick().expect("tick succeeds");
        assert!(Instant::now() < deadline, "first frame deadline");
        std::thread::sleep(Duration::from_millis(1));
    }
    media
}

fn open_on(sink: String, header: Header) -> DecodedMedia {
    open_with(header, sink, None, false)
}

fn audio_header(edge: u32) -> Header {
    Header {
        width: edge,
        height: edge,
        audio: true,
        ..test_header(0)
    }
}

struct Playback {
    shown: u64,
    heard: Option<u64>,
}

/// Ticks once; the playhead may neither lead the sink nor jump back.
fn observe(media: &DecodedMedia, last: &mut u64) -> Playback {
    media.tick().expect("tick succeeds");
    let shown = media.timestamp() as u64;
    let heard = media
        .imp()
        .audio
        .borrow()
        .as_ref()
        .and_then(PcmOutput::position_us);
    assert!(
        shown + 40_000 >= *last,
        "playhead rewound from {last} to {shown}"
    );
    if let Some(heard) = heard {
        assert!(
            shown <= heard + 100_000,
            "playhead {shown} ran ahead of the sink at {heard}"
        );
    }
    *last = shown;
    std::thread::sleep(Duration::from_millis(1));
    Playback { shown, heard }
}

#[test]
fn audio_and_video_play_through_a_sink_that_starts_late() {
    crate::test_support::gtk_test(
        "ui::media::tests::audio_and_video_play_through_a_sink_that_starts_late",
        late_sink_playback,
    );
}

fn late_sink_playback() {
    // 600 ms is far more than the three-frame presentation queue holds.
    for (edge, depth) in [
        (0, PRESENTATION_QUEUE + 1..=usize::MAX),
        (16, 0..=PRESENTATION_QUEUE),
    ] {
        let media = open_on(model_sink(600, 33_333), audio_header(edge));
        let imp = media.imp();
        let deadline = Instant::now() + Duration::from_secs(10);
        let (mut last, mut deepest) = (0, 0);
        while observe(&media, &mut last).shown < 900_000 {
            deepest = deepest.max(imp.frames.borrow().len());
            assert!(
                Instant::now() < deadline,
                "{edge}px: late sink playback deadline"
            );
        }
        assert_eq!(
            imp.recoveries.get(),
            0,
            "{edge}px: a late start is not a stall"
        );
        assert!(
            depth.contains(&deepest),
            "{edge}px: queued {deepest} frames"
        );
        media.close();
    }
}

#[test]
fn a_sink_slower_than_the_stuck_timeout_is_not_a_stall() {
    crate::test_support::gtk_test(
        "ui::media::tests::a_sink_slower_than_the_stuck_timeout_is_not_a_stall",
        slow_start_is_not_a_stall,
    );
}

fn slow_start_is_not_a_stall() {
    let clock = ManualClock::new();
    let media = open_with(
        audio_header(0),
        "fakesink sync=true".into(),
        Some(clock.clone().upcast()),
        false,
    );
    let imp = media.imp();
    // The sink has not run yet, so the stuck timer must not count this wait.
    let until = Instant::now() + AUDIO_STUCK_TIMEOUT + Duration::from_millis(300);
    let mut last = 0;
    while Instant::now() < until {
        assert_eq!(
            observe(&media, &mut last).shown,
            0,
            "playhead moved before the sink"
        );
        assert_eq!(
            imp.recoveries.get(),
            0,
            "waiting for the sink is not a stall"
        );
    }
    let started = Instant::now();
    let deadline = started + Duration::from_secs(10);
    loop {
        clock.set_time(started.elapsed().as_micros() as u64);
        if observe(&media, &mut last).shown >= 100_000 {
            break;
        }
        assert!(Instant::now() < deadline, "manual clock playback deadline");
    }
    assert_eq!(imp.recoveries.get(), 0);
    media.close();
}

#[test]
fn a_mid_play_stall_keeps_the_samples_the_sink_will_still_play() {
    crate::test_support::gtk_test(
        "ui::media::tests::a_mid_play_stall_keeps_the_samples_the_sink_will_still_play",
        stall_keeps_unplayed_samples,
    );
}

fn stall_keeps_unplayed_samples() {
    let clock = ManualClock::new();
    let media = open_with(
        audio_header(0),
        "fakesink sync=true".into(),
        Some(clock.clone().upcast()),
        true,
    );
    let imp = media.imp();
    let deadline = Instant::now() + Duration::from_secs(10);
    let (started, mut last) = (Instant::now(), 0);
    loop {
        clock.set_time(started.elapsed().as_micros() as u64);
        if observe(&media, &mut last).shown >= 300_000 {
            break;
        }
        assert!(Instant::now() < deadline, "manual clock playback deadline");
    }
    // Freeze the sink for longer than the retained second while the playhead runs on.
    let base = imp.clock_base.get();
    let until = Instant::now() + Duration::from_millis(1_500);
    while Instant::now() < until {
        media.tick().expect("tick succeeds");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(imp.recoveries.get(), 0);
    let mut window = [0.0; 4];
    let kept = imp.history.borrow().as_ref().is_some_and(|history| {
        history.window_ending_at(base * media::SAMPLE_RATE / 1_000_000 + 4, &mut window)
    });
    assert!(kept, "samples at the stalled sink position were trimmed");
    media.close();
}

#[test]
fn resume_waits_for_the_sink_instead_of_leading_and_snapping_back() {
    crate::test_support::gtk_test(
        "ui::media::tests::resume_waits_for_the_sink_instead_of_leading_and_snapping_back",
        resume_waits_for_the_sink,
    );
}

fn resume_waits_for_the_sink() {
    let clock = ManualClock::new();
    let media = open_with(
        audio_header(0),
        "fakesink sync=true".into(),
        Some(clock.clone().upcast()),
        false,
    );
    let imp = media.imp();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = 0;
    let hold = |last: &mut u64, base: u64| {
        let until = Instant::now() + Duration::from_millis(150);
        let mut shown = None;
        let mut ticks = 0;
        while Instant::now() < until {
            let playback = observe(&media, last);
            assert!(playback.heard.is_none_or(|heard| heard <= base));
            let shown = *shown.get_or_insert(playback.shown);
            assert_eq!(playback.shown, shown, "playhead moved before the sink");
            ticks += 1;
        }
        assert!(ticks > 10, "held for only {ticks} ticks");
        shown.expect("observed playback")
    };
    let run = |last: &mut u64, from: u64, until_shown: u64| {
        let started = Instant::now();
        loop {
            clock.set_time(from + started.elapsed().as_micros() as u64);
            if observe(&media, last).shown >= until_shown {
                break;
            }
            assert!(Instant::now() < deadline, "manual clock playback deadline");
        }
    };

    assert_eq!(hold(&mut last, 0), 0, "playback starts where the sink is");
    run(&mut last, 0, 300_000);

    media.pause();
    let paused_at = imp.position.get();
    let base = imp.clock_base.get();
    media.play();
    let resumed_at = hold(&mut last, base);
    assert!(
        resumed_at <= paused_at && paused_at - resumed_at <= 40_000,
        "resumed at {resumed_at} after pausing at {paused_at}"
    );
    run(&mut last, base, base + 300_000);
    assert_eq!(imp.recoveries.get(), 0);
    media.close();
}

mod manual_clock {
    use super::*;
    use gst::subclass::prelude::*;
    use std::sync::{Condvar, Mutex};

    /// A pipeline clock that only moves when a test sets its time.
    #[derive(Default)]
    pub struct Imp {
        time: Mutex<(u64, u64)>,
        changed: Condvar,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Imp {
        const NAME: &'static str = "StrataManualClock";
        type Type = ManualClock;
        type ParentType = gst::Clock;
    }

    impl ObjectImpl for Imp {}
    impl GstObjectImpl for Imp {}

    impl ClockImpl for Imp {
        fn internal_time(&self) -> gst::ClockTime {
            gst::ClockTime::from_nseconds(self.time.lock().expect("clock").0)
        }

        fn wait(
            &self,
            id: &gst::ClockId,
        ) -> (
            Result<gst::ClockSuccess, gst::ClockError>,
            gst::ClockTimeDiff,
        ) {
            let target = id.time().nseconds();
            let mut time = self.time.lock().expect("clock");
            let generation = time.1;
            while time.0 < target && time.1 == generation {
                time = self.changed.wait(time).expect("clock");
            }
            if time.1 != generation {
                return (Err(gst::ClockError::Unscheduled), 0);
            }
            (Ok(gst::ClockSuccess::Ok), time.0 as i64 - target as i64)
        }

        // The sink is the only waiter, so any unschedule releases it.
        fn unschedule(&self, _id: &gst::ClockId) {
            self.time.lock().expect("clock").1 += 1;
            self.changed.notify_all();
        }
    }

    glib::wrapper! {
        pub struct ManualClock(ObjectSubclass<Imp>) @extends gst::Clock, gst::Object;
    }

    impl ManualClock {
        pub fn new() -> Self {
            glib::Object::new()
        }

        pub fn set_time(&self, micros: u64) {
            let imp = self.imp();
            imp.time.lock().expect("clock").0 = micros * 1_000;
            imp.changed.notify_all();
        }
    }
}
use manual_clock::ManualClock;

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
            assert_eq!(imp.origin_samples.get(), 0, "the timeline keeps its origin");
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
                media.imp().origin_samples.get(),
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
fn audio_timestamps_stay_sample_exact_from_any_start_tick_and_across_a_rate_change() {
    crate::test_support::gtk_test(
        "ui::media::tests::audio_timestamps_stay_sample_exact_from_any_start_tick_and_across_a_rate_change",
        || {
            let media = DecodedMedia::new(test_source("/audio-timestamps"));
            // What the audio output demands of each chunk: its exact sample position.
            let expected = |samples: u64| samples * 1_000_000 / media::SAMPLE_RATE;
            for start in [0, 1, 2, 3, 107_851, 107_852] {
                for fps in [30, 60] {
                    let first = start * (fps / media::FPS);
                    media
                        .imp()
                        .origin_samples
                        .set(media::samples_at(first, fps));
                    let per_tick = media::SAMPLE_RATE / u64::from(fps);
                    for n in 0..200 {
                        assert_eq!(
                            media.audio_timestamp(first + n, fps),
                            expected(u64::from(n) * per_tick),
                            "start {start} at {fps} fps, tick {n}"
                        );
                    }
                }
            }

            let start = 107_851;
            media.imp().origin_samples.set(media::samples_at(start, 30));
            let switch = (start + 40) * 2;
            for (n, samples) in [(0, 0), (1, 800), (2, 1_600)] {
                assert_eq!(
                    media.audio_timestamp(switch + n, 60),
                    expected(40 * 1_600 + samples),
                    "a 60 fps decoder continues where the 30 fps one stopped"
                );
            }
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
