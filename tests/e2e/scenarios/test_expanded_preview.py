# SPDX-License-Identifier: MIT
"""Shift+Space enlarges the quick preview; the same live preview moves back and forth."""

from __future__ import annotations

import re
import subprocess

import pytest
from PIL import Image

from harness.fixtures import FixtureTree

TIME = re.compile(r"^(\d+):(\d{2})\s*/\s*(\d+):(\d{2})$")


@pytest.fixture
def fixture_tree(test_environment):
    fixture = FixtureTree.create(
        {
            "a-notes.txt": "the quick brown fox\n",
            "b-notes.txt": "jumps over the lazy dog\n",
        }
    )
    Image.new("RGB", (320, 180), "green").save(fixture.path("c-picture.png"))
    subprocess.run(
        [
            "ffmpeg", "-hide_banner", "-loglevel", "error",
            "-f", "lavfi", "-i", "testsrc2=size=1280x720:rate=30",
            "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000",
            "-t", "8", "-c:v", "libx264", "-preset", "veryfast", "-crf", "32", "-threads", "1",
            "-pix_fmt", "yuv420p",
            "-c:a", "aac", str(fixture.path("d-clip.mp4")),
        ],
        check=True, capture_output=True, timeout=60,
    )
    try:
        yield fixture
    finally:
        fixture.cleanup()


def media_seconds(strata):
    preview = strata.preview()
    if preview is None:
        return None
    for node in preview.find_all(rendered=False):
        match = TIME.match(node.text or node.name)
        if match:
            return int(match.group(1)) * 60 + int(match.group(2))
    return None


def test_shift_space_enlarges_the_preview_and_escape_returns_it(strata):
    strata.select_entry_with_keyboard("a-notes.txt")
    strata.keyboard.press("space")
    strata.wait(lambda: strata.preview_shows("the quick brown fox"), "the small preview")
    small = strata.preview().screen_bounds()

    strata.keyboard.press("shift+space")
    strata.wait(
        lambda: strata.preview() is not None
        and strata.preview().screen_bounds().width > small.width * 1.4,
        "the preview to fill most of the window",
    )
    assert strata.preview_shows("the quick brown fox"), "the same document is still shown"

    strata.keyboard.press("Escape")
    strata.wait(
        lambda: strata.preview() is not None
        and strata.preview().screen_bounds().width <= small.width + 4,
        "the preview to return to the drawer",
    )
    assert strata.preview_shows("the quick brown fox")
    strata.keyboard.press("Escape")
    strata.wait(lambda: strata.preview() is None, "a second Escape to close the preview")


def test_plain_arrows_change_file_while_the_preview_stays_expanded(strata):
    strata.select_entry_with_keyboard("a-notes.txt")
    strata.keyboard.press("shift+space")
    strata.wait(lambda: strata.preview_shows("the quick brown fox"), "the expanded preview")
    strata.keyboard.press("Down")
    strata.wait(
        lambda: strata.preview_shows("jumps over the lazy dog"),
        "the next file in the same expanded view",
    )
    assert not strata.preview_shows("the quick brown fox")
    assert strata.focused_name() == "b-notes.txt"
    strata.keyboard.press("Up")
    strata.wait(lambda: strata.preview_shows("the quick brown fox"), "the previous file")
    strata.keyboard.press("shift+space")
    strata.wait(lambda: strata.preview() is not None, "the drawer")


def test_shift_space_in_the_filter_field_still_types_a_space(strata):
    strata.select_entry_with_keyboard("a-notes.txt")
    strata.keyboard.press("ctrl+f")
    field = strata.wait(
        lambda: strata.window.find(role="text", states={"editable", "focused"}),
        "the filter field",
    )
    strata.keyboard.type_text("a")
    strata.keyboard.press("shift+space")
    strata.keyboard.type_text("n")
    strata.wait(lambda: field.text == "a n", "the filter field to hold 'a n'")
    assert strata.preview() is None, "typing in the filter never expands anything"


@pytest.mark.preferences(preview_autoplay=True)
def test_a_playing_video_keeps_its_place_across_expanding_and_collapsing(strata):
    strata.select_entry_with_keyboard("d-clip.mp4")
    strata.keyboard.press("space")
    strata.wait(lambda: strata.preview_shows("/0:08"), "the sandboxed video")
    strata.wait(lambda: (media_seconds(strata) or 0) >= 1, "playback to start")

    seen = [media_seconds(strata)]

    def advanced(baseline):
        seconds = media_seconds(strata)
        if seconds is not None:
            seen.append(seconds)
        return seconds is not None and seconds >= baseline + 1

    strata.keyboard.press("shift+space")
    strata.wait(lambda: advanced(seen[0]), "playback to go on when expanded")
    expanded_at = seen[-1]
    strata.keyboard.press("shift+space")
    strata.wait(lambda: advanced(expanded_at), "playback to go on when collapsed")

    assert seen == sorted(seen), f"the playhead never moved back: {seen}"
    assert not strata.preview_shows("Preview unavailable")
    assert not strata.preview_shows("busy")
