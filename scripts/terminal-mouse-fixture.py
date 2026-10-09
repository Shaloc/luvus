#!/usr/bin/env python3
"""Bounded mouse-aware terminal for the isolated remote-session smoke test."""
import json
import os
from pathlib import Path
import select
import sys
import termios
import time
import tty


def main():
    root = Path(os.environ["LUVUS_SMOKE_ROOT"]).resolve()
    repo = Path(__file__).resolve().parent.parent
    assert root.parent == repo / "target" and root.name.startswith("remote-smoke-")
    assert (root / ".isolated-smoke").read_text() == str(root)
    assert sys.argv[1] in ("local", "remote")
    report = root / ("mouse-" + sys.argv[1] + ".json")
    previous = termios.tcgetattr(0)
    received = bytearray()

    def record():
        pending = report.with_suffix(".pending")
        pending.write_text(json.dumps({"received": received.hex()}))
        pending.replace(report)

    try:
        tty.setraw(0)
        record()
        os.write(1, b"\x1b[?1002h\x1b[?1006h\x1b]2;INPUT_OWNER_PROOF\x07"
                    b"\x1b[0m\x1b[2J\x1b[HMOUSE_COPY_PROOF\r\n")
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            if not select.select([0], [], [], 0.2)[0]:
                continue
            chunk = os.read(0, 4096)
            if not chunk or chunk == b"q":
                break
            received.extend(chunk)
            assert len(received) < 4096
            record()
    finally:
        os.write(1, b"\x1b[?1002l\x1b[?1006l\x1b]2;\x07")
        termios.tcsetattr(0, termios.TCSANOW, previous)


if __name__ == "__main__":
    main()
