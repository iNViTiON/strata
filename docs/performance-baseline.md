# Performance Baseline

This document defines the reproducible local performance check for Strata. Results are machine-specific and should be compared against earlier runs on the same system rather than treated as universal benchmarks.

## Fixtures

Generate deterministic mixed directories containing 1,000, 10,000, and 100,000 entries, plus deep-tree and native-path edge cases:

```bash
./scripts/generate-fixture.sh target/fixtures --clean
```

Omit `--clean` to fill in missing fixture entries without deleting existing ones. On the baseline machine, clean generation of all fixtures takes approximately one second.

## Instrumented run

Build once so compilation is excluded from startup measurements:

```bash
cargo build --release
RUST_LOG=strata=debug target/release/strata target/fixtures/100000
```

Default logs contain request IDs, backend names, counts, and timings without browsed locations.
`RUST_LOG=strata=debug` explicitly enables diagnostic logging and may include full native paths.
Remote URI user-info, authentication parameters, queries, and fragments remain redacted at every
level. Review diagnostic logs before sharing them.

Strata accepts a startup directory on the command line. Structured logs report:

- Time until the application window is presented
- First provider batch latency
- Time until the first directory batch is rendered
- Complete enumeration time and entry count
- Per-batch UI append duration
- Cancelled directory request IDs

Capture sampled RSS and proportional set size (PSS) with:

```bash
STRATA_BINARY=target/release/strata \
  ./scripts/profile-fixture.sh target/fixtures/100000
```

Close other Strata instances before profiling so GApplication does not forward the request to an existing process. PSS is included because RSS charges each process for shared GTK, graphics, and font pages in full.

## Initial baseline — 2026-08-29

Environment:

- AMD Ryzen 9 9950X3D, 60 GiB RAM
- Omarchy, Wayland/Hyprland
- GTK 4.22.4
- Rust 1.97.1
- Optimized `--release` build
- Warm filesystem cache

Each timing below is a single engineering sample. “First UI batch” includes process startup; the parenthesized value is the time spent appending that 128-entry batch.

| Fixture | Window presented | First provider batch | First UI batch | Complete enumeration | Peak RSS / PSS |
|---:|---:|---:|---:|---:|---:|
| 1,000 | 90 ms | 72 ms | 105 ms (2.5 ms) | 104 ms | 241.9 / 147.9 MiB |
| 10,000 | 89 ms | 67 ms | 103 ms (2.3 ms) | 287 ms | 247.4 / 153.2 MiB |
| 100,000 | 91 ms | 68 ms | 107 ms (2.3 ms) | 2,131 ms | 278.0 / 183.9 MiB |

### Findings

- Initial interaction is effectively independent of directory size because results arrive in bounded 128-entry batches.
- UI batch insertion is well below one 16.7 ms frame in these samples.
- Removing the UI’s duplicate `FileEntry` storage reduced the 100,000-entry sample from approximately 315 MiB RSS / 219 MiB PSS to 278 MiB RSS / 184 MiB PSS.
- The 100,000-entry memory result still exceeds the provisional 150 MiB target and remains an open optimization item. The current model retains one application entry and one GTK string object per result.
- Complete enumeration scales approximately linearly and remains asynchronous, but smooth scrolling must also be evaluated manually under sustained input.
- After globally stable incremental sorting was introduced, the 100,000-entry sample completed in 3,755 ms at 286.3 MiB RSS / 191.4 MiB PSS. Individual GTK insertion batches remained below 4 ms, but the application-side merge cost is a future optimization target.

## Regression budgets

Use these provisional guardrails:

- The first provider batch should remain below 100 ms on the target machine.
- A 100,000-entry directory must remain scrollable while enumeration continues.
- UI batch rendering should not routinely exceed one 16.7 ms frame.
- Navigation away from a loading fixture must cancel its request without stale rows appearing.
- Long-term peak memory remains targeted below 150 MiB for the 100,000-entry fixture.

Record hardware, build profile, GTK version, cache state, and notable environmental load whenever replacing the baseline table. Use multiple samples before treating small timing differences as regressions.

## Preview start latency — 2026-09-29

How long moving the selection to another file takes to show its preview, with and
without **Preload neighbor previews** (see
[Neighbor preloading](preview-sandbox.md#neighbor-preloading)).

Method: the pinned e2e container image (rootless Podman), private Xvfb 1440×900
with the Cairo renderer, `--release` (no LTO), software decoding, Intel Core Ultra 7
258V. One fresh application per file type, 13 distinct copies of one synthetic
fixture (FFmpeg `testsrc2`, cairo PDFs; these compress better than camera files),
stepped with the Down key. Times run from the key press to the next painted frame
(images, PDFs) or the first decoded frame (video, audio), from temporary timing
marks that are not part of the build. The first sample is a cold start and is left
out; medians and p90 cover the other twelve. The page cache was warm and the
machine was shared, so run-to-run differences of up to about 2× occur: compare
only rows measured together. The first-frame time that stays in the build is the
`latency_ms` field of the `sandboxed media first frame` debug log
(`RUST_LOG=strata::ui::media=debug`).

| File | Preference off (median / p90 ms) | On, after a 1.5 s pause | On, 0.35 s between key presses |
| --- | --- | --- | --- |
| JPEG 2 MP | 186 / 266 | 8 / 13 | |
| JPEG 24 MP | 575 / 966 | 8 / 12 | 530 / 598 |
| PNG 4K | 389 / 565 | 8 / 12 | |
| WebP 2 MP | 231 / 240 | 11 / 19 | |
| PDF, 1 page | 147 / 177 | 8 / 15 | |
| PDF, 50 pages | 154 / 162 | 10 / 13 | 10 / 14 |
| H.264 1080p | 284 / 318 | 9 / 13 | 87 / 101 |
| HEVC 4K | 419 / 538 | 11 / 13 | |
| VP9 720p | 399 / 521 | 7 / 12 | |
| MOV 720p | 410 / 478 | 11 / 14 | |
| MP3 | 259 / 328 | 10 / 14 | |
| FLAC | 240 / 253 | 11 / 15 | |
| GIF | 330 / 386 | 8 / 13 | |

Where the time goes without preloading (H.264 1080p, decode size 449×252, medians
of a separate run): 80 ms focus debounce, 8 ms request and render, 11 ms wait for
the next 8 ms player tick, 84 ms bubblewrap and helper start (the helper is the
whole Strata executable), 123 ms `ffprobe` inside the sandbox, 191 ms FFmpeg start
and first frame, 10 ms audio output setup on the GTK thread; 512 ms in total. Image
renders spend 200 to 580 ms in a pool job; a first PDF page spends 120 to 160 ms in a
one-shot sandbox. The first video in a process also pays for creating the GStreamer
audio output: 190 to 1300 ms here, where the container has no audio server.

Memory of one parked worker (a paused preview, measured six seconds after its first
frame):

| | Software, whole process tree (PSS / RSS) | VA-API (host FFmpeg only) |
| --- | --- | --- |
| H.264 1080p | +105 / +163 MiB | video FFmpeg 56 / 79 MiB, audio FFmpeg 13 / 36 MiB, GPU-mapped 35 MiB |
| HEVC 4K | +236 / +284 MiB | video FFmpeg 170 / 193 MiB, audio FFmpeg 12 / 35 MiB, GPU-mapped 114 MiB |
| VP9 720p | +60 / +91 MiB | |
| MP3 | +43 / +75 MiB | |

A parked worker uses no CPU beyond a 50 ms wake-up of its reader thread. Reaching
that state costs 120 to 590 ms of CPU per video. VA-API and the desktop's audio
server could not be measured inside the container image, so the VA-API column
comes from FFmpeg alone.
