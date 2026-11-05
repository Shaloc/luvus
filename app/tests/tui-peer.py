"""Drive only a marker-checked disposable TUI inside a private pseudo-terminal."""
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import signal
import struct
import sys
import termios
import time

home = Path(os.environ["LUVUS_HOME"]).resolve(strict=True)
assert (home / ".isolated-app").read_text() == str(home)
binary = Path(sys.argv[1]).resolve(strict=True)
pid, master = pty.fork()
if pid == 0:
    os.execv(str(binary), [str(binary), "--session", "matrix"])

def resize(cols, rows):
    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))

try:
    resize(72, 24)
    announced = False
    pending = bytearray()
    seen_tail = False
    seen_notice = False
    commands = b""
    while True:
        ready, _, _ = select.select([master, 0], [], [])
        if master in ready:
            data = os.read(master, 65536)
            if not data:
                break
            pending.extend(data)
            del pending[:-65536]
            if not announced:
                if b"MIXED_BASELINE" in pending:
                    announced = True
                    print("painted", flush=True)
            if not seen_tail and b"LIVE_TAIL_ONLY" in pending:
                seen_tail = True
                print("live-tail", flush=True)
            if not seen_notice and b"MixedNotice" in pending:
                seen_notice = True
                print("notice", flush=True)
        if 0 in ready:
            chunk = os.read(0, 4096)
            if not chunk:
                break
            commands += chunk
            while b"\n" in commands:
                line, commands = commands.split(b"\n", 1)
                command = json.loads(line)
                if "resize" in command:
                    resize(*command["resize"])
                elif "input" in command:
                    os.write(master, command["input"].encode())
finally:
    os.close(master)
    try:
        os.kill(pid, signal.SIGTERM)
        until = time.monotonic() + 2
        while time.monotonic() < until:
            if os.waitpid(pid, os.WNOHANG)[0]:
                break
            time.sleep(0.025)
        else:
            os.kill(pid, signal.SIGKILL)
            os.waitpid(pid, 0)
    except (ProcessLookupError, ChildProcessError):
        pass
