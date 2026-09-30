// SPDX-License-Identifier: MIT

use std::{
    io::{self, Read, Write},
    os::fd::AsFd,
    time::{Duration, Instant},
};

use crate::{sandbox::Cancellation, services::MediaPreviewSize};

/// Ticks per second of the seek grid: where a decode starts, and where decoders
/// of different frame rates line up. The drawer decodes at this rate as well.
pub(crate) const FPS: u32 = 30;
/// The fastest rate a decoder produces. Both rates divide a second of 48 kHz
/// audio evenly, and every 30 fps tick is a whole number of 60 fps ticks.
pub(crate) const MAX_FPS: u32 = 60;
// The terminal tick must fit the wire format at any rate; this also denotes unknown duration.
pub(crate) const MAX_DURATION_US: u64 = u32::MAX as u64 * 1_000_000 / MAX_FPS as u64;
pub(crate) const SAMPLE_RATE: u64 = 48_000;
const AUDIO_BYTES_PER_SECOND: usize = SAMPLE_RATE as usize * 4;
pub(crate) const STARTUP_TIMEOUT: Duration = Duration::from_secs(22);
pub(crate) const FRAME_TIMEOUT: Duration = Duration::from_secs(8);
pub(crate) const HEADER_BYTES: usize = 48;
const MAGIC: &[u8; 8] = b"STRRAW02";

/// A tick on the seek grid, in microseconds.
pub(crate) fn timestamp(tick: u32) -> u64 {
    timestamp_at(tick, FPS)
}

/// A tick of a decoder running at `fps`, in microseconds.
pub(crate) fn timestamp_at(tick: u32, fps: u32) -> u64 {
    u64::from(tick) * 1_000_000 / u64::from(fps)
}

pub(crate) fn is_frame_rate(fps: u32) -> bool {
    fps == FPS || fps == MAX_FPS
}

/// Above this many pixels a frame is too large to carry through the decoder
/// pipe at 60 a second, so the decoder stays at the seek-grid rate: resolution
/// comes before frame rate.
pub(crate) const FAST_FRAME_PIXELS: u64 = 2560 * 1440;

/// The rate to decode a `width` x `height` frame at: the source's own, as far as
/// the screen shows it and the pipe carries it.
pub(crate) fn frame_rate_for(width: u32, height: u32, native_fps: u32, max_fps: u32) -> u32 {
    if u64::from(width) * u64::from(height) > FAST_FRAME_PIXELS {
        FPS
    } else {
        native_fps.min(max_fps).max(FPS)
    }
}

pub(crate) fn seek_tick(time_us: u64, duration_us: u64) -> u32 {
    (time_us
        .min(duration_us.saturating_sub(1))
        .min(MAX_DURATION_US - 1)
        * u64::from(FPS)
        / 1_000_000) as u32
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Header {
    pub width: u32,
    pub height: u32,
    pub audio: bool,
    pub duration_us: u64,
    /// In ticks of this decoder's own rate.
    pub start_tick: u32,
    /// Frames per second this decoder produces: one frame per tick.
    pub fps: u32,
    /// The rate the source itself plays at, rounded to one of the two.
    pub native_fps: u32,
}

impl Header {
    pub fn video_bytes(self) -> usize {
        self.width as usize * self.height as usize * 4
    }

    pub fn audio_bytes(self) -> usize {
        if self.audio {
            AUDIO_BYTES_PER_SECOND / self.fps.max(1) as usize
        } else {
            0
        }
    }

    pub fn ticks(self) -> u32 {
        (self.duration_us * u64::from(self.fps)).div_ceil(1_000_000) as u32
    }

    /// The whole duration in ticks of the seek grid.
    pub fn seek_ticks(self) -> u32 {
        (self.duration_us * u64::from(FPS)).div_ceil(1_000_000) as u32
    }

    pub fn validate(self, size: MediaPreviewSize, start_tick: u32) -> io::Result<Self> {
        if self.width > size.width as u32
            || self.height > size.height as u32
            || self.width > MediaPreviewSize::MAX_EXPANDED_EDGE as u32
            || self.height > MediaPreviewSize::MAX_EXPANDED_EDGE as u32
            || (self.width == 0) != (self.height == 0)
            || (self.width == 0 && !self.audio)
            || self.duration_us == 0
            || self.duration_us > MAX_DURATION_US
            || !is_frame_rate(self.fps)
            || !is_frame_rate(self.native_fps)
            || self.fps > frame_rate_for(self.width, self.height, self.native_fps, size.max_fps)
            || start_tick.checked_mul(self.fps / FPS) != Some(self.start_tick)
            || self.start_tick >= self.ticks()
        {
            return Err(invalid("Invalid decoded-media header"));
        }
        Ok(self)
    }

    pub fn read(
        reader: &mut impl Read,
        size: MediaPreviewSize,
        start_tick: u32,
    ) -> io::Result<Self> {
        let mut bytes = [0; HEADER_BYTES];
        reader.read_exact(&mut bytes)?;
        if &bytes[..8] != MAGIC || u32_at(&bytes, 20) > 1 || u32_at(&bytes, 44) != 0 {
            return Err(invalid("Unknown decoded-media format"));
        }
        let header = Self {
            width: u32_at(&bytes, 8),
            height: u32_at(&bytes, 12),
            audio: u32_at(&bytes, 20) == 1,
            duration_us: u64_at(&bytes, 24),
            start_tick: u32_at(&bytes, 32),
            fps: u32_at(&bytes, 36),
            native_fps: u32_at(&bytes, 40),
        }
        .validate(size, start_tick)?;
        if header.width.checked_mul(4) != Some(u32_at(&bytes, 16)) {
            return Err(invalid("Invalid decoded-frame stride"));
        }
        Ok(header)
    }

    pub fn write(self, writer: &mut impl Write) -> io::Result<()> {
        writer.write_all(MAGIC)?;
        for value in [
            self.width,
            self.height,
            self.width * 4,
            u32::from(self.audio),
        ] {
            writer.write_all(&value.to_le_bytes())?;
        }
        writer.write_all(&self.duration_us.to_le_bytes())?;
        writer.write_all(&self.start_tick.to_le_bytes())?;
        writer.write_all(&self.fps.to_le_bytes())?;
        writer.write_all(&self.native_fps.to_le_bytes())?;
        writer.write_all(&0_u32.to_le_bytes())
    }
}

#[derive(Debug)]
pub(crate) struct Frame {
    pub tick: u32,
    pub pixels: Vec<u8>,
    pub samples: Vec<u8>,
}

impl Frame {
    /// `fps` is the rate of the decoder the frame's tick counts in.
    pub fn write(&self, writer: &mut impl Write, fps: u32) -> io::Result<()> {
        write_record(
            writer,
            1,
            self.tick,
            timestamp_at(self.tick, fps),
            self.pixels.len(),
            self.samples.len(),
        )?;
        writer.write_all(&self.pixels)?;
        writer.write_all(&self.samples)
    }
}

pub(crate) fn write_end(writer: &mut impl Write, tick: u32, duration_us: u64) -> io::Result<()> {
    write_record(writer, 0, tick, duration_us, 0, 0)
}

fn write_record(
    writer: &mut impl Write,
    kind: u32,
    tick: u32,
    pts: u64,
    video: usize,
    audio: usize,
) -> io::Result<()> {
    writer.write_all(&kind.to_le_bytes())?;
    writer.write_all(&tick.to_le_bytes())?;
    writer.write_all(&pts.to_le_bytes())?;
    writer.write_all(&(video as u32).to_le_bytes())?;
    writer.write_all(&(audio as u32).to_le_bytes())
}

pub(crate) struct Decoder {
    pub header: Header,
    next_tick: u32,
    ended: bool,
}

pub(crate) enum Packet {
    Frame(Frame),
    End(u64),
}

impl Decoder {
    pub fn new(header: Header) -> Self {
        Self {
            header,
            next_tick: header.start_tick,
            ended: false,
        }
    }

    pub fn read(&mut self, reader: &mut impl Read) -> io::Result<Packet> {
        let mut bytes = [0; 24];
        reader.read_exact(&mut bytes)?;
        let kind = u32_at(&bytes, 0);
        let tick = u32_at(&bytes, 4);
        let pts = u64_at(&bytes, 8);
        let video = u32_at(&bytes, 16) as usize;
        let audio = u32_at(&bytes, 20) as usize;
        if self.ended || tick != self.next_tick {
            return Err(invalid("Out-of-order decoded frame"));
        }
        if kind == 0 {
            if video != 0
                || audio != 0
                || tick == self.header.start_tick
                || pts <= timestamp_at(tick - 1, self.header.fps)
                || pts > self.header.duration_us
                || pts > timestamp_at(tick, self.header.fps)
            {
                return Err(invalid("Invalid decoded-media end"));
            }
            self.ended = true;
            return Ok(Packet::End(pts));
        }
        if kind != 1
            || tick >= self.header.ticks()
            || pts != timestamp_at(tick, self.header.fps)
            || video != self.header.video_bytes()
            || audio != self.header.audio_bytes()
        {
            return Err(invalid(
                "Invalid decoded-frame dimensions, length or timestamp",
            ));
        }
        let mut pixels = vec![0; video];
        let mut samples = vec![0; audio];
        reader.read_exact(&mut pixels)?;
        reader.read_exact(&mut samples)?;
        self.next_tick += 1;
        Ok(Packet::Frame(Frame {
            tick,
            pixels,
            samples,
        }))
    }
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("fixed wire field"),
    )
}

fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(
        bytes[offset..offset + 8]
            .try_into()
            .expect("fixed wire field"),
    )
}

pub(crate) fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub(crate) struct TimedReader<'a, F> {
    pub fd: &'a F,
    pub deadline: Instant,
    pub cancellation: &'a Cancellation,
}

impl<F: AsFd> Read for TimedReader<'_, F> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        use rustix::event::{PollFd, PollFlags, Timespec, poll};
        loop {
            if self.cancellation.is_cancelled() {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "Preview cancelled",
                ));
            }
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Decoded-media progress timed out",
                ));
            }
            let mut fds = [PollFd::new(self.fd, PollFlags::IN)];
            let timeout = Timespec {
                tv_sec: 0,
                tv_nsec: remaining.min(Duration::from_millis(20)).as_nanos() as i64,
            };
            if poll(&mut fds, Some(&timeout))? != 0 {
                match rustix::io::read(self.fd, &mut *buffer) {
                    Err(rustix::io::Errno::INTR | rustix::io::Errno::AGAIN) => continue,
                    result => return result.map_err(Into::into),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
