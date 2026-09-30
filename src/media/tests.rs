// SPDX-License-Identifier: MIT

use super::*;
use std::{io::Cursor, os::unix::net::UnixStream, thread};

fn header() -> Header {
    Header {
        width: 16,
        height: 16,
        audio: true,
        duration_us: 30_000_000,
        start_tick: 0,
        fps: 30,
        native_fps: 30,
    }
}
fn bytes() -> Vec<u8> {
    let mut bytes = Vec::new();
    header().write(&mut bytes).expect("header");
    bytes
}
fn parse(bytes: &[u8]) -> io::Result<Header> {
    Header::read(&mut Cursor::new(bytes), MediaPreviewSize::new(640, 480), 0)
}

#[test]
fn headers_reject_unknown_formats_dimensions_strides_flags_and_timeline() {
    assert_eq!(parse(&bytes()).expect("valid header"), header());
    for (offset, value) in [
        (8, 0_u32),
        (8, 641),
        (8, u32::MAX),
        (12, 481),
        (16, 63),
        (20, 2),
        (32, 1),
        (36, 0),
        (36, 45),
        (36, 120),
        (40, 24),
        (44, 1),
    ] {
        let mut bad = bytes();
        bad[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        assert!(parse(&bad).is_err(), "offset {offset}, value {value}");
    }
    for duration in [0_u64, MAX_DURATION_US + 1, u64::MAX] {
        let mut bad = bytes();
        bad[24..32].copy_from_slice(&duration.to_le_bytes());
        assert!(parse(&bad).is_err());
    }
    let mut bad = bytes();
    bad[7] = b'1';
    assert!(parse(&bad).is_err());
    for length in 0..HEADER_BYTES {
        assert!(parse(&bytes()[..length]).is_err());
    }
}

#[test]
fn a_large_frame_is_accepted_only_for_a_request_that_asked_for_it() {
    let large = Header {
        width: 2600,
        height: 1600,
        ..header()
    };
    let mut bytes = Vec::new();
    large.write(&mut bytes).expect("header");
    let read = |size| Header::read(&mut Cursor::new(&bytes), size, 0);
    assert_eq!(
        read(MediaPreviewSize::expanded(2600, 1600)).expect("asked for"),
        large
    );
    assert!(read(MediaPreviewSize::expanded(1300, 800)).is_err());
    assert!(read(MediaPreviewSize::new(2600, 1600)).is_err());
}

#[test]
fn a_sixty_frame_decoder_ticks_twice_as_fast_on_the_same_seek_grid() {
    let fast = Header {
        fps: 60,
        native_fps: 60,
        start_tick: 10,
        ..header()
    };
    let mut bytes = Vec::new();
    fast.write(&mut bytes).expect("header");
    let read = |size, start| Header::read(&mut Cursor::new(&bytes), size, start);
    let wide = MediaPreviewSize::expanded(640, 480);
    assert_eq!(
        read(wide, 5).expect("seek tick 5 is tick 10 at 60 fps"),
        fast
    );
    assert!(read(wide, 10).is_err(), "the start is on the seek grid");
    assert!(
        read(MediaPreviewSize::new(640, 480), 5).is_err(),
        "not asked for"
    );
    assert!(
        read(wide.for_refresh_rate(30_000), 5).is_err(),
        "a slower screen"
    );

    assert_eq!(fast.audio_bytes() * 2, header().audio_bytes());
    assert_eq!(fast.ticks(), header().ticks() * 2);
    assert_eq!(timestamp_at(2, 60), timestamp(1));

    let mut frame = Vec::new();
    Frame {
        tick: 10,
        pixels: vec![0; fast.video_bytes()],
        samples: vec![0; fast.audio_bytes()],
    }
    .write(&mut frame, 60)
    .expect("frame");
    assert!(matches!(
        Decoder::new(fast).read(&mut Cursor::new(&frame)),
        Ok(Packet::Frame(_))
    ));
    assert!(
        Decoder::new(Header {
            fps: 30,
            start_tick: 10,
            ..fast
        })
        .read(&mut Cursor::new(&frame))
        .is_err(),
        "audio and timestamp belong to the frame's own rate"
    );
}

#[test]
fn frames_are_length_checked_before_allocation_and_must_be_contiguous() {
    let mut bytes = Vec::new();
    Frame {
        tick: 0,
        pixels: vec![1; 1024],
        samples: vec![2; header().audio_bytes()],
    }
    .write(&mut bytes, 30)
    .expect("frame");
    for (offset, value) in [
        (0, 2_u32),
        (4, 1),
        (16, 1023),
        (16, u32::MAX),
        (20, 0),
        (20, u32::MAX),
    ] {
        let mut bad = bytes[..24].to_vec();
        bad[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        assert!(Decoder::new(header()).read(&mut Cursor::new(bad)).is_err());
    }
    let mut bad = bytes.clone();
    bad[8] = 1;
    assert!(Decoder::new(header()).read(&mut Cursor::new(bad)).is_err());
    for length in [0, 23, 24, 24 + 1023, bytes.len() - 1] {
        assert!(
            Decoder::new(header())
                .read(&mut Cursor::new(&bytes[..length]))
                .is_err()
        );
    }
    let mut decoder = Decoder::new(header());
    assert!(matches!(
        decoder.read(&mut Cursor::new(&bytes)),
        Ok(Packet::Frame(_))
    ));
    assert!(decoder.read(&mut Cursor::new(&bytes)).is_err());
    let mut end = Vec::new();
    write_end(&mut end, 1, timestamp(1)).expect("end");
    assert!(matches!(
        decoder.read(&mut Cursor::new(&end)),
        Ok(Packet::End(_))
    ));
    assert!(decoder.read(&mut Cursor::new(end)).is_err());
}

#[test]
fn full_length_seeks_and_frames_stop_at_the_source_end_without_tick_overflow() {
    for duration in [31_000_000, 3_600_000_000, MAX_DURATION_US] {
        let ticks = (duration * u64::from(FPS)).div_ceil(1_000_000) as u32;
        for requested in [duration, u64::MAX] {
            assert_eq!(seek_tick(requested, duration), ticks - 1);
        }
        let h = Header {
            duration_us: duration,
            start_tick: ticks - 1,
            ..header()
        };
        let mut encoded = Vec::new();
        h.write(&mut encoded).expect("header");
        let h = Header::read(
            &mut Cursor::new(encoded),
            MediaPreviewSize::new(640, 480),
            ticks - 1,
        )
        .expect("long timeline");
        let mut decoder = Decoder::new(h);
        let mut bytes = Vec::new();
        Frame {
            tick: ticks - 1,
            pixels: vec![0; h.video_bytes()],
            samples: vec![0; h.audio_bytes()],
        }
        .write(&mut bytes, 30)
        .expect("last frame");
        assert!(matches!(
            decoder.read(&mut Cursor::new(bytes)),
            Ok(Packet::Frame(_))
        ));
        let mut bytes = Vec::new();
        Frame {
            tick: ticks,
            pixels: vec![0; h.video_bytes()],
            samples: vec![0; h.audio_bytes()],
        }
        .write(&mut bytes, 30)
        .expect("extra frame");
        assert!(decoder.read(&mut Cursor::new(bytes)).is_err());
        let mut bytes = Vec::new();
        write_end(&mut bytes, ticks, duration + 1).expect("invalid end");
        assert!(decoder.read(&mut Cursor::new(bytes)).is_err());
        let mut bytes = Vec::new();
        write_end(&mut bytes, ticks, duration).expect("source end");
        assert!(
            matches!(decoder.read(&mut Cursor::new(bytes)), Ok(Packet::End(end)) if end == duration)
        );
    }
    assert_eq!(seek_tick(32_000_000, 3_600_000_000), 960);
    assert_eq!(seek_tick(u64::MAX, u64::MAX), u32::MAX / 2);
}

#[test]
fn partial_reads_and_silent_workers_obey_deadlines_and_cancellation() {
    let (read, mut write) = UnixStream::pair().expect("private pipe");
    write.write_all(b"a").expect("partial record");
    let cancellation = Cancellation::default();
    let started = Instant::now();
    let mut reader = TimedReader {
        fd: &read,
        deadline: started + Duration::from_millis(50),
        cancellation: &cancellation,
    };
    assert_eq!(
        reader.read_exact(&mut [0; 2]).expect_err("deadline").kind(),
        io::ErrorKind::TimedOut
    );
    assert!(started.elapsed() < Duration::from_secs(1));
    let cancel = cancellation.clone();
    let worker = thread::spawn(move || {
        thread::sleep(Duration::from_millis(20));
        cancel.cancel();
    });
    reader.deadline = Instant::now() + Duration::from_secs(10);
    let started = Instant::now();
    assert_eq!(
        reader
            .read_exact(&mut [0; 1])
            .expect_err("cancelled")
            .kind(),
        io::ErrorKind::ConnectionAborted
    );
    assert!(started.elapsed() < Duration::from_secs(1));
    worker.join().expect("cancel thread");
}
