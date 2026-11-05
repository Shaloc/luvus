#!/usr/bin/env python3
"""Opt-in SSH desktop fixture. Only operate on a freshly marked build under target/."""
import http.server
import base64
import socket
import errno
import json
import os
from pathlib import Path
import re
import shlex
import signal
import sys
import threading
import subprocess
import time
import hashlib


def checked(root):
    root = Path(root).resolve(strict=True)
    if (root.parent.name != "target" or not re.fullmatch(r"app-e2e-real-[A-Za-z0-9]+", root.name)
            or (root / ".isolated-app").read_text().strip() != str(root)
            or root.stat().st_uid != os.getuid() or root.stat().st_mode & 0o077):
        raise ValueError("refusing an unmarked owner")
    return root


def ssh_display():
    # Replace only the remote command with our marked fixture entrypoint.
    # Discovery and -D execute the real OpenSSH programs and actual host config.
    local = Path(os.environ["APP_SSH_LOCAL"]).resolve()
    if local.parent != Path(__file__).resolve().parents[2] / "target" or not local.name.startswith("app-e2e-ssh-"):
        raise ValueError("invalid display fixture")
    config = json.loads((local / ".isolated-ssh").read_text())
    args = sys.argv[1:]
    host = config["host"]
    if args.count(host) != 1 or "-R" in args or "-L" in args:
        raise ValueError("unexpected SSH route")
    i = args.index(host)
    role = args[i + 1:]
    display = False
    options = list(config["options"])
    if config.get("registered"):
        if role == ["luvus", "--version", "--remote-session-protocol"]:
            operation = role[1:]
        elif role == ["luvus", "--session", "default", "remote-session-list"]:
            operation = role[3:]
        elif role[:3] == ["luvus", "--session", "ssh"] and role[3:] in (
                ["remote-control-bridge", "--existing"], ["remote-client-bridge", "--existing"]):
            operation = role[3:]
            display = operation[0] == "remote-client-bridge"
        else:
            raise ValueError("unexpected registered fixture operation: " + repr(role))
        if "-S" in args:
            socket = args[args.index("-S") + 1]
            pool = local / "state/ssh-pool" / hashlib.sha256(host.encode()).hexdigest()[:24]
            allowed = set()
            for name in ["commands.sock", *[f"master-{n}.sock" for n in range(64)]]:
                logical = str(pool / name)
                if len(os.fsencode(logical)) >= 100:
                    digest = 0xcbf29ce484222325
                    for byte in os.fsencode(logical):
                        digest = ((digest ^ byte) * 0x100000001b3) & 0xffffffffffffffff
                    logical = f"/private/tmp/luvus-{os.getuid()}/{digest:016x}-ssh.sock"
                allowed.add(logical)
            if socket not in allowed:
                raise ValueError("refusing a non-fixture SSH master")
            options = [arg for pair in zip(options[::2], options[1::2])
                       if pair[1] not in ("ControlMaster=no", "ControlPath=none") for arg in pair]
        args[i + 1:] = [shlex.join(["python3", config["root"] + "/real-owner.py", "cli", *operation])]
    elif role == ["luvus", "--session", "ssh", "remote-client-bridge"]:
        args[i + 1:] = [shlex.join(["python3", config["root"] + "/real-owner.py", "cli", "remote-client-bridge"])]
    elif args[i + 1:] or not ("-G" in args or "-D" in args and "-N" in args):
        raise ValueError("unexpected SSH operation")
    with (local / "ssh-routes.jsonl").open("a") as log:
        log.write(json.dumps(args) + "\n")
    if display:
        # The fault switch can terminate only the subprocess this fixture owns.
        # Never kill a ControlMaster, the owner server, or an unrelated SSH client.
        fault = local / "hold-display"
        if fault.exists():
            raise SystemExit(255)
        child = subprocess.Popen(["/usr/bin/ssh", *options, *args])
        signal.signal(signal.SIGTERM, lambda *_: child.terminate())
        try:
            while child.poll() is None:
                if fault.exists():
                    child.terminate()
                    break
                time.sleep(0.025)
            raise SystemExit(child.wait(timeout=5))
        finally:
            if child.poll() is None:
                child.kill()
                child.wait()
    # OpenSSH constructs ProxyJump's -W child from argv[0]. Keep the actual
    # executable so a hop never recurses through the fixture command wrapper.
    os.execv("/usr/bin/ssh", ["/usr/bin/ssh", *options, *args])


class Origin(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        proxy = self.server.proxy
        if not proxy and self.path == "/socket" and self.headers.get("Upgrade","").lower() == "websocket":
            key = self.headers["Sec-WebSocket-Key"]
            accept = base64.b64encode(hashlib.sha1((key+"258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest()).decode()
            self.send_response(101)
            self.send_header("Upgrade","websocket")
            self.send_header("Connection","Upgrade")
            self.send_header("Sec-WebSocket-Accept",accept)
            self.end_headers()
            self.wfile.write(b"\x81\x08OWNER_WS")
            return
        if proxy and (self.headers.get("Proxy-Authorization") != "Basic dGVzdDp0ZXN0"
                      or not self.path.startswith("http://owner-only.invalid/")):
            self.send_error(403)
            return
        payload = bytes(range(256)) * 10
        attachment = self.path.endswith("/download")
        if not attachment:
            label = "OWNER PROXY" if proxy else "OWNER LOCALHOST"
            payload = ('<!doctype html><meta charset="utf-8"><title>' + label + '</title><h1>' + label +
                       '</h1><input id="name" aria-label="Name"><button id="greet" onclick="document.getElementById(\'result\').textContent=\'Hello \'+document.getElementById(\'name\').value">Greet</button><p id="result"></p>'
                       '<input type="file" id="upload"><a href="/download" download>Download</a>').encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/octet-stream" if attachment else "text/html")
        if attachment:
            self.send_header("Content-Disposition", 'attachment; filename="remote.bin"')
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, *_args):
        pass


def main():
    if Path(sys.argv[0]).name == "ssh":
        ssh_display()
    root = checked(Path(__file__).resolve().parent)
    mode = sys.argv[1]
    if mode == "serve":
        signal.alarm(300)
        servers = [http.server.ThreadingHTTPServer(("127.0.0.1", 0), Origin) for _ in range(2)]
        class IPv6Server(http.server.ThreadingHTTPServer):
            address_family=socket.AF_INET6
        try:
            servers.append(IPv6Server(("::1",0),Origin))
        except OSError as error:
            if error.errno not in (errno.EAFNOSUPPORT, errno.EADDRNOTAVAIL):
                raise
        for server, proxy in zip(servers, [False, True, False]):
            server.proxy = proxy
            threading.Thread(target=server.serve_forever, daemon=True).start()
        ports = dict(zip(["origin", "proxy","ipv6"], [s.server_port for s in servers]))
        (root / "ports.json").write_text(json.dumps(ports))
        print(json.dumps(ports), flush=True)
        while opcode := sys.stdin.buffer.read(1):
            if opcode != b"p":
                break
            servers[1].shutdown()
            servers[1].server_close()
            print("proxy-stopped",flush=True)
        return
    ports = json.loads((root / "ports.json").read_text()) if (root / "ports.json").exists() else None
    for key in list(os.environ):
        if key.startswith("LUVUS_") or key.lower() in ("http_proxy", "https_proxy", "all_proxy", "no_proxy"):
            del os.environ[key]
    os.environ.update(HOME=str(root / "home"), LUVUS_HOME=str(root / "state"),
                      XDG_CONFIG_HOME=str(root / "home/config"), XDG_CACHE_HOME=str(root / "home/cache"),
                      XDG_DATA_HOME=str(root / "home/data"), XDG_STATE_HOME=str(root / "home/state"),
                      SHELL="/bin/sh", TERM="xterm-256color", LUVUS_SESSION="ssh")
    if ports:
        os.environ.update(HTTP_PROXY=f'http://test:test@127.0.0.1:{ports["proxy"]}', NO_PROXY="localhost,::1")
    os.chdir(root / "workspace")
    binary = root / "build/debug/luvus"
    if binary.resolve(strict=True) != binary:
        raise ValueError("not the fixture executable")
    if mode == "cli":
        os.execv(binary, [str(binary), "--session", "ssh", *sys.argv[2:]])
    if mode == "cdp":
        os.environ["LUVUS_APP_BINARY"] = str(binary)
        os.execvp("node", ["node", str(root / "owner-cdp.mjs"), *sys.argv[2:]])
    raise ValueError("unknown operation")


if __name__ == "__main__":
    main()
