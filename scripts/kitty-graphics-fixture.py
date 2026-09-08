#!/usr/bin/env python3
"""Small Kitty client used only inside test-remote-sessions.py's private PTYs."""
import base64
import json
import os
from pathlib import Path
import select
import struct
import sys
import termios
import time
import tty
import zlib

root = Path(os.environ["LUVUS_SMOKE_ROOT"]).resolve()
repo = Path(__file__).resolve().parent.parent
assert root.parent == repo / "target" and (root / ".isolated-smoke").read_text() == str(root)
label = sys.argv[1]
assert label in ("local", "remote")
report = root / ("graphics-" + label + ".json")
saved = termios.tcgetattr(0)


def read_for(seconds):
    result = bytearray()
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if select.select([0], [], [], 0.02)[0]:
            result.extend(os.read(0, 4096))
    return bytes(result)


def render(color):
    # Incompressible raw data exercises multiple APC chunks as well as PTY
    # read boundaries. The second image exercises the zlib direct-transfer path.
    raw = bytes(color) * (64 * 64)
    encoded = base64.b64encode(raw if label == "local" else zlib.compress(raw))
    compression = "" if label == "local" else ",o=z"
    os.write(1, b"\x1b[2J\x1b[1;1H")
    for offset in range(0, len(encoded), 4096):
        chunk = encoded[offset:offset + 4096]
        more = int(offset + 4096 < len(encoded))
        header = (f"a=T,i=7,f=32,s=64,v=64,c=16,r=8,C=1,q=2,m={more}{compression}"
                  if offset == 0 else f"m={more},q=2")
        os.write(1, b"\x1b_G" + header.encode() + b";" + chunk + b"\x1b\\")


# These synthetic protocol cases are authored here; no tele code or assets are
# imported. Full-image virtual resources plus independently positioned marker
# cells exercise the protocol used by inline-photo terminal applications.
virtual_ids = (0x010207, 0x020308)
diacritics = tuple(chr(n) for n in (
    0x0305, 0x030D, 0x030E, 0x0310, 0x0312, 0x033D, 0x033E, 0x033F,
    0x0346, 0x034A, 0x034B, 0x034C, 0x0350, 0x0351, 0x0352, 0x0357,
))


def png_image(second):
    def chunk(kind, data):
        return (struct.pack(">I", len(data)) + kind + data
                + struct.pack(">I", zlib.crc32(kind + data)))

    rows = bytearray()
    for row in range(64):
        rows.append(0)  # PNG filter: none.
        for col in range(64):
            color = ((0, 0, 255, 255) if row < 32 else (255, 255, 0, 255)) if second else (
                (255, 0, 0, 255) if col < 32 else (0, 255, 0, 255))
            rows.extend(color)
    # Uncompressed DEFLATE deliberately makes this PNG cross the protocol's
    # 4096-byte transfer chunks while leaving exact-color screenshot assertions.
    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", 64, 64, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(bytes(rows), level=0))
            + chunk(b"IEND", b""))


def upload_virtual():
    os.write(1, b"\x1b_Ga=d,d=I,i=7,q=2;\x1b\\")
    for second, image_id in enumerate(virtual_ids):
        encoded = base64.b64encode(png_image(second))
        for offset in range(0, len(encoded), 4096):
            more = int(offset + 4096 < len(encoded))
            header = (f"a=T,i={image_id},f=100,t=d,U=1,c=16,r=8,q=2,m={more}"
                      if offset == 0 else f"m={more},q=2")
            os.write(1, b"\x1b_G" + header.encode() + b";"
                     + encoded[offset:offset + 4096] + b"\x1b\\")


def virtual_markers(window=False, deleted=False, clear=True):
    # ED erases text, not virtual resources. Returning to the full window must
    # therefore work without another image upload.
    if clear:
        os.write(1, b"\x1b[2J")
    h_off, v_off, cols, rows = (8, 4, 8, 4) if window else (0, 0, 16, 8)
    for position, image_id in enumerate(virtual_ids):
        if deleted and position == 0:
            continue
        foreground = f"\x1b[38;2;{image_id >> 16};{(image_id >> 8) & 255};{image_id & 255}m"
        for row in range(rows):
            cells = "".join("\U0010eeee" + diacritics[v_off + row] + diacritics[h_off + col]
                            for col in range(cols))
            os.write(1, (f"\x1b[{row + 2};{2 + position * 24}H"
                         + foreground + cells + "\x1b[0m").encode())
    receipt = "XXXXXXXX" if deleted else "WWWWWWWW" if window else "VVVVVVVV"
    os.write(1, b"\x1b[20;1H" + receipt.encode())


try:
    tty.setraw(0)
    os.write(1, b"\x1b_Gi=4207,a=q,t=d,f=24,s=1,v=1;AAAA\x1b\\\x1b[16t\x1b[c")
    reply = read_for(1)
    report.write_text(json.dumps({"ack": b"Gi=4207;OK" in reply,
                                  "cell_size": b"[6;16;8t" in reply,
                                  "reply": repr(reply)}))
    os.write(1, b"\x1b[?1049h")
    render((255, 0, 0, 255) if label == "local" else (0, 0, 255, 255))
    while True:
        key = os.read(0, 4096)
        if b"q" in key:
            break
        if b"g" in key:
            render((0, 255, 0, 255))
        if b"v" in key:
            upload_virtual()
            virtual_markers()
        if b"w" in key:
            virtual_markers(window=True)
        if b"f" in key:
            # Text-only reappearance exercises partial-damage fallback after
            # the previous frame had no visible images or display resources.
            virtual_markers(clear=False)
        if b"h" in key:
            os.write(1, b"\x1b[2J\x1b[20;1HHHHHHHHH")
        if b"x" in key:
            os.write(1, f"\x1b_Ga=d,d=I,i={virtual_ids[0]},q=2;\x1b\\".encode())
            virtual_markers(window=True, deleted=True)
        for trigger, command, receipt in [
            (b"u", b"\x1b[4S", b"UUUUUUUU"),
            (b"r", b"\x1b[4T", b"RRRRRRRR"),
            (b"e", b"\x1b[24S", b"EEEEEEEE"),
        ]:
            if trigger in key:
                # SU/SD exercise terminal page scrolling, independent of a
                # whole-canvas application replacing its last image.
                os.write(1, command + b"\x1b[20;1H" + receipt)
        if b"c" in key:
            os.write(1, b"\x1b_Ga=d,d=A,q=2;\x1b\\\x1b[2J")
finally:
    os.write(1, b"\x1b[?1049l")
    termios.tcsetattr(0, termios.TCSANOW, saved)
