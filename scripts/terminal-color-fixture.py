#!/usr/bin/env python3
"""Bounded OSC color probe for test-remote-sessions.py's isolated PTYs only."""

import json
import os
from pathlib import Path
import re
import select
import sys
import termios
import time
import tty


def main():
    root = Path(os.environ["LUVUS_SMOKE_ROOT"]).resolve()
    assert root.parent == Path(__file__).resolve().parent.parent / "target"
    assert (root / ".isolated-smoke").read_text() == str(root)
    label = sys.argv[1]
    assert label in ("local", "merged", "direct")
    report = root / ("colors-" + label + ".json")
    previous = termios.tcgetattr(0)
    revision = 0

    def probe():
        nonlocal revision
        os.write(1, b"\x1b]10;?\x07\x1b]11;?\x1b\\")
        received = bytearray()
        colors = {}
        deadline = time.monotonic() + 2
        while time.monotonic() < deadline and len(colors) < 2:
            if not select.select([0], [], [], 0.02)[0]:
                continue
            received.extend(os.read(0, 4096))
            assert len(received) < 16384
            for match in re.finditer(rb"\x1b](10|11);rgb:([a-fA-F0-9]+)/([a-fA-F0-9]+)/([a-fA-F0-9]+)(?:\x07|\x1b\\)", received):
                colors[match[1].decode()] = [int(value, 16) * 255 // ((1 << (4 * len(value))) - 1)
                                           for value in match.group(2, 3, 4)]
        # Like clients that require a complete default palette, don't use a
        # partial OSC 11 reply to select a light diff theme.
        light = len(colors) == 2 and sum(colors["11"]) > 3 * 128
        diff = (230, 255, 237) if light else (0, 95, 0)
        output = ("\x1b[0m\x1b[2J\x1b[H"
                  "COLOR_DEFAULT\r\n"
                  f"\x1b[48;2;{diff[0]};{diff[1]};{diff[2]}mCOLOR_DIFF\x1b[0m\r\n"
                  "COLOR_RESET\r\n\x1b[32mCOLOR_INDEX\x1b[0m\r\n")
        os.write(1, output.encode())
        revision += 1
        temporary = report.with_suffix(".pending")
        temporary.write_text(json.dumps({"revision": revision, "colors": colors, "diff": diff}))
        temporary.replace(report)

    try:
        tty.setraw(0)
        probe()
        while True:
            key = os.read(0, 1)
            if key in (b"q", b"", b"\x03"):
                break
            if key == b"d":
                os.write(1, b"\x1b]10;#abcdef\x07\x1b]11;#123456\x07\x1b]4;2;#246824\x07")
            elif key == b"r":
                os.write(1, b"\x1b]110\x07\x1b]111\x07\x1b]104;2\x07")
            elif key != b"p":
                continue
            probe()
    finally:
        os.write(1, b"\x1b[0m\x1b]110\x07\x1b]111\x07\x1b]104;2\x07")
        termios.tcsetattr(0, termios.TCSANOW, previous)


if __name__ == "__main__":
    main()
