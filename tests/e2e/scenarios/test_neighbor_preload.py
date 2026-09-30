# SPDX-License-Identifier: MIT
"""Neighbor previews are prepared in the background and promoted when selected."""

import re
import subprocess

import pytest
from PIL import Image

from harness.fixtures import FixtureTree

PRELOAD_LOG = "neighbor preview requested"
PARKED_FRAME_LOG = "sandboxed media first frame parked=true"
PROMOTED_LOG = "parked neighbor preview promoted"
FRESH_FRAME_LOG = "sandboxed media first frame parked=false"


@pytest.fixture
def test_environment(test_environment, monkeypatch):
    variables = test_environment.variables
    monkeypatch.setattr(
        test_environment,
        "variables",
        lambda: {
            **variables(),
            "NO_COLOR": "1",
            "RUST_LOG": "strata=info,strata::ui::preview::preload=debug,strata::ui::media=debug",
        },
    )
    return test_environment


@pytest.fixture
def fixture_tree():
    fixture = FixtureTree.create({})
    for name in ("clip-a.mp4", "clip-b.mp4", "clip-c.mp4"):
        subprocess.run(
            ["ffmpeg", "-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i",
             "testsrc2=size=320x180:rate=30", "-t", "2", "-c:v", "libx264", "-threads", "1",
             "-pix_fmt", "yuv420p", "-an", str(fixture.path(name))],
            check=True, capture_output=True, timeout=60,
        )
    for index, name in enumerate(("photo-a.png", "photo-b.png", "photo-c.png")):
        Image.new("RGB", (400, 300), (40 + index * 60, 120, 200)).save(fixture.path(name))
    try:
        yield fixture
    finally:
        fixture.cleanup()


ANSI = re.compile(r"\x1b\[[0-9;]*m")


def _log(strata):
    return ANSI.sub("", strata.application.log())


@pytest.mark.preferences(
    browser_mode="list",
    single_click_previews=False,
    hardware_accelerated_video_previews=False,
    preload_neighbor_previews=True,
)
def test_a_neighboring_video_is_parked_then_promoted_without_a_new_decode(strata):
    strata.select_entry_with_keyboard("clip-a.mp4")
    strata.keyboard.press("space")
    strata.wait(lambda: strata.preview_shows("/0:02"), "the first video preview")
    strata.wait(lambda: PRELOAD_LOG in _log(strata), "the neighbor to be requested")
    strata.wait(lambda: PARKED_FRAME_LOG in _log(strata), "the neighbor's first frame")
    assert _log(strata).count(FRESH_FRAME_LOG) == 1

    strata.keyboard.press("Down")
    strata.wait(lambda: PROMOTED_LOG in _log(strata), "the parked neighbor to be promoted")
    strata.wait(lambda: strata.preview_shows("clip-b.mp4"), "the neighbor's preview")
    strata.wait(lambda: strata.preview_shows("/0:02"), "the promoted video duration")
    assert not strata.preview_shows("Preview unavailable")
    # No new decode began for clip-b: its first frame came from the parked worker.
    assert _log(strata).count(FRESH_FRAME_LOG) == 1


@pytest.mark.preferences(
    browser_mode="list",
    single_click_previews=False,
    preload_neighbor_previews=True,
)
def test_neighboring_images_are_prepared_and_shown_from_the_cache(strata):
    strata.select_entry_with_keyboard("photo-a.png")
    strata.keyboard.press("space")
    strata.wait(lambda: strata.preview_shows("photo-a.png"), "the first image preview")
    strata.wait(lambda: "neighbor preview requested kind=Image" in _log(strata), "the neighbor image")
    strata.keyboard.press("Down")
    strata.wait(lambda: strata.preview_shows("photo-b.png"), "the neighbor image preview")
    assert not strata.preview_shows("Preview unavailable")


@pytest.mark.preferences(
    browser_mode="list",
    single_click_previews=False,
    hardware_accelerated_video_previews=False,
)
def test_nothing_is_prepared_unless_the_preference_is_on(strata):
    strata.select_entry_with_keyboard("clip-a.mp4")
    strata.keyboard.press("space")
    strata.wait(lambda: strata.preview_shows("/0:02"), "the first video preview")
    strata.keyboard.press("Down")
    strata.wait(lambda: strata.preview_shows("clip-b.mp4"), "the next preview")
    strata.wait(lambda: strata.preview_shows("/0:02"), "the next video duration")
    log = _log(strata)
    assert PRELOAD_LOG not in log
    assert PARKED_FRAME_LOG not in log
    assert PROMOTED_LOG not in log
