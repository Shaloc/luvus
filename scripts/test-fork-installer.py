#!/usr/bin/env python3
"""Exercise install.sh offline, only inside a private fixture under target/.

Optional argv[1] is a checkout binary for a real installed-runtime smoke.
No production configuration, binary, server, or network is accessed.
"""
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import shlex
import select
import sys
import tarfile
import tempfile
import time


def ssh_helper():
    repo = Path(__file__).resolve().parent.parent
    root = Path(os.environ["FIXTURE_ROOT"]).resolve()
    assert root.parent == repo / "target" and root.name.startswith("installer-smoke-")
    args = sys.argv[2:]
    while args and args[0].startswith("-"):
        args = args[2:] if args[0] == "-o" else args[1:]
    host, *command = args
    assert host in ("fixture", "denied"), host
    with (root / "ssh-calls").open("a") as log:
        log.write(json.dumps(command) + "\n")
    if host == "denied":
        print("Permission denied (publickey)", file=sys.stderr)
        return 255
    remote_home = root / "remote-home"
    env = {k: v for k, v in os.environ.items() if not k.startswith("LUVUS_")}
    env.update(HOME=str(remote_home), LUVUS_HOME=str(root / "remote-state"))
    for suffix in ("CONFIG", "CACHE", "DATA", "STATE"):
        env[f"XDG_{suffix}_HOME"] = str(remote_home / suffix.lower())
    installed = remote_home / ".local/bin/luvus"
    if len(command) == 1 and command[0].startswith("sh -c "):
        parsed = shlex.split(command[0])
        assert parsed == ["sh", "-c", "unset LUVUS_VERSION LUVUS_INSTALL_DIR;\n" + (repo / "install.sh").read_text()]
        with (root / "installs").open("a") as log:
            log.write(host + "\n")
        return subprocess.call(parsed, env=env)
    # A stale PATH binary remains stale after installation; standard fallback
    # must discover the compatible user-local binary without any PATH edits.
    if command == ["luvus", "--version", "--remote-session-protocol"]:
        print("luvus 1.0.99")
        return 0
    assert len(command) == 1 and command[0].startswith("for luvus_bin in "), command
    if not installed.exists():
        return 127
    # Do not evaluate the production fallback's other absolute paths in this
    # fixture. Execute only the installed fixture binary with the allowed role.
    print("LUVUS_STANDARD_FALLBACK", file=sys.stderr, flush=True)
    if "remote-session-list" in command[0]:
        argv = ["--session", "default", "remote-session-list"]
    else:
        assert "--version" in command[0]
        argv = ["--version", "--remote-session-protocol"]
    return subprocess.call([str(installed), *argv], env=env)


def host_admission_smoke(repo, root, binary, env):
    local_home = root / "local-home"
    (local_home / ".ssh").mkdir(parents=True)
    (root / "remote-home").mkdir()
    (root / "remote-state").mkdir()
    (local_home / ".ssh/config").write_text("Host fixture denied\n  HostName 192.0.2.1\n")
    config_file = root / "state/config.json"
    baseline = {"check_updates": False, "shell": "/bin/sh", "remote_hosts": [], "language": "ja", "prefix": "ctrl+b"}
    config_file.write_text(json.dumps(baseline))
    helper = root / "bin/ssh"
    helper.write_text(f"#!/bin/sh\nexec {shlex.quote(sys.executable)} {shlex.quote(str(Path(__file__).resolve()))} --ssh-helper \"$@\"\n")
    helper.chmod(0o755)
    env = dict(env, HOME=str(local_home))
    for suffix in ("CONFIG", "CACHE", "DATA", "STATE"):
        env[f"XDG_{suffix}_HOME"] = str(local_home / suffix.lower())
    installed = root / "remote-home/.local/bin/luvus"

    def run(*args, ok=True):
        result = subprocess.run([str(binary), *args], env=env, cwd=root,
                                capture_output=True, text=True, timeout=30)
        assert (result.returncode == 0) == ok, result.stdout + result.stderr
        assert not list((root / "remote-state").rglob("server.pid"))
        assert not list((root / "state").rglob("server.pid"))
        return result

    run("host", "add", "unknown", "--install", ok=False)
    assert json.loads(config_file.read_text()) == baseline
    assert not (root / "ssh-calls").exists()
    run("host", "add", "denied", "--install", ok=False)
    assert not (root / "installs").exists()
    run("host", "add", "fixture", ok=False)
    assert not installed.exists() and not (root / "installs").exists()
    config = json.loads(config_file.read_text())
    assert config["remote_hosts"] == ["denied", "fixture"] and config["language"] == "ja"
    run("session", "list", "--json")
    assert not (root / "installs").exists(), "read-only discovery installed a binary"
    status = json.loads(run("host", "add", "fixture", "--install", "--json").stdout)
    assert status["type"] == "host_added" and status["host"] == "fixture"
    assert installed.read_bytes() == binary.read_bytes()
    assert (root / "installs").read_text().splitlines() == ["fixture"]
    run("host", "add", "fixture", "--install")
    assert (root / "installs").read_text().splitlines() == ["fixture"], "compatible build reinstalled"
    installed.unlink()  # Only the validated fixture installation, never a production binary.
    config["remote_auto_install"] = True
    config_file.write_text(json.dumps(config))
    run("host", "add", "fixture")
    assert installed.read_bytes() == binary.read_bytes()
    assert len((root / "installs").read_text().splitlines()) == 2
    print("PASS: host add explicit admission, default-off, installed policy, auth/alias rejection, fallback, no server lifecycle")
    # Exercise explicit UI admission through a real client and detached owner,
    # in exactly this fixture home. Only this newly created test owner is stopped.
    import fcntl
    import pty
    import struct
    import termios
    installed.unlink()
    config_file.write_text(json.dumps(dict(baseline, language="en")))
    owner = [str(binary), "--session", "admission-ui"]
    subprocess.run([*owner, "server", "start"], env=env, cwd=root, check=True, capture_output=True, timeout=15)
    pid_file = root / "state/sessions/admission-ui/server.pid"
    pid_before = pid_file.read_bytes()
    client = None
    master, slave = pty.openpty()
    try:
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 120, 0, 0))
        client = subprocess.Popen(owner, env=dict(env, TERM="xterm-256color"), cwd=root,
                                  stdin=slave, stdout=slave, stderr=slave, start_new_session=True,
                                  preexec_fn=lambda: fcntl.ioctl(0, termios.TIOCSCTTY, 0))
        os.close(slave)
        slave = None

        def drain(seconds):
            output = bytearray()
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                if select.select([master], [], [], 0.05)[0]:
                    output.extend(os.read(master, 65536))
            return bytes(output)

        drain(3)
        os.write(master, b"\x02=8")
        screen = drain(1)
        assert b"Auto-install" in screen, "Remote policy was not rendered"
        os.write(master, b"\x1b[B\r")
        drain(0.5)
        assert json.loads(config_file.read_text())["remote_auto_install"]
        assert not installed.exists(), "policy checkbox alone installed a binary"
        os.write(master, b"\x1b[B\x1b[B\r")  # Skip denied, select fixture in sorted SSH aliases.
        deadline = time.monotonic() + 12
        while time.monotonic() < deadline and not installed.exists():
            drain(0.2)
        assert installed.is_file(), "explicit Settings host selection did not install"
        assert installed.read_bytes() == binary.read_bytes()
        assert json.loads(config_file.read_text())["remote_hosts"] == ["fixture"]
        assert len((root / "installs").read_text().splitlines()) == 3
        assert pid_file.read_bytes() == pid_before and client.poll() is None
        assert not list((root / "remote-state").rglob("server.pid"))
        print("PASS: real Settings checkbox only persists policy; selecting host installs, existing UI owner survives")
    finally:
        if client is not None:
            client.terminate()
            client.wait(timeout=5)
        os.close(master)
        if slave is not None:
            os.close(slave)
        subprocess.run([*owner, "server", "stop"], env=env, cwd=root, check=True, capture_output=True, timeout=15)


def main():
    repo = Path(__file__).resolve().parent.parent
    root = Path(tempfile.mkdtemp(prefix="installer-smoke-", dir=repo / "target"))
    for directory in ("bin", "downloads", "install", "tmp", "state"):
        (root / directory).mkdir()
    payload = b'#!/bin/sh\n[ "$*" = "--version --remote-session-protocol" ] || exit 90\nprintf "luvus 1.0.99 remote-session=2 transport=9\\n"\n'
    if len(sys.argv) > 1:
        binary = Path(sys.argv[1]).resolve()
        assert binary.is_relative_to(repo / "target"), "only test a checkout build"
        payload = binary.read_bytes()
    package = "luvus-1.0.99-0123456789ab"
    for target in ("x86_64-unknown-linux-musl", "aarch64-apple-darwin"):
        archive = root / "downloads" / f"luvus-{target}.tar.gz"
        with tarfile.open(archive, "w:gz") as tar:
            info = tarfile.TarInfo(f"{package}-{target}/luvus")
            info.mode, info.size = 0o755, len(payload)
            tar.addfile(info, io.BytesIO(payload))
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        archive.with_suffix(".gz.sha256").write_text(f"{digest}  {archive.name}\n")
    helpers = {
        "uname": '#!/bin/sh\ncase "$1" in -s) echo "$FIXTURE_OS" ;; -m) echo "$FIXTURE_ARCH" ;; *) exit 99 ;; esac\n',
        "curl": '''#!/bin/sh
printf '%s\\n' "$*" >> "$FIXTURE_ROOT/requests"
destination=""
while [ "$#" -gt 1 ]; do
  case "$1" in
    -fsSL) shift ;;
    --connect-timeout|--max-time|-w) shift 2 ;;
    -o) destination="$2"; shift 2 ;;
    *) exit 96 ;;
  esac
done
case "$1" in
  https://api.github.com/repos/Shaloc/luvus/releases/latest)
    [ "${FIXTURE_API_FAILURE:-0}" = 0 ] || exit 22
    printf '{"tag_name":"fork-1.0.99-0123456"}\\n' ;;
  https://github.com/Shaloc/luvus/releases/latest)
    printf '%s' "${FIXTURE_REDIRECT:-https://github.com/Shaloc/luvus/releases/tag/fork-1.0.99-0123456}" ;;
  *) case "$1" in
    https://github.com/Shaloc/luvus/releases/download/fork-1.0.99-0123456/luvus-*)
      cp "$FIXTURE_ROOT/downloads/${1##*/}" "$destination" ;;
    *) exit 98 ;;
  esac ;;
esac
''',
    }
    for name, source in helpers.items():
        path = root / "bin" / name
        path.write_text(source)
        path.chmod(0o755)
    env = {k: v for k, v in os.environ.items() if not k.startswith("LUVUS_")}
    env.update(PATH=f"{root}/bin:{env['PATH']}", LUVUS_INSTALL_DIR=str(root / "install"),
               LUVUS_HOME=str(root / "state"), TMPDIR=str(root / "tmp"),
               FIXTURE_ROOT=str(root), FIXTURE_OS="Linux", FIXTURE_ARCH="x86_64")
    installed = root / "install/luvus"

    def run(ok=True, **extra):
        result = subprocess.run(["sh", str(repo / "install.sh")], env=dict(env, **extra),
                                text=True, capture_output=True, timeout=30)
        assert (result.returncode == 0) == ok, result.stdout + result.stderr
        assert not list((root / "tmp").iterdir()), "download fixture leaked"
        return result.stdout + result.stderr

    installed.write_bytes(b"old binary")
    old_inode = installed.stat().st_ino
    assert "No servers were restarted" in run()
    assert installed.read_bytes() == payload and installed.stat().st_ino != old_inode
    assert any(p.read_bytes() == b"old binary" for p in (root / "install").glob(".luvus-update.*/luvus.previous"))
    assert "installed" in run(LUVUS_VERSION="fork-1.0.99-0123456").lower()
    assert installed.read_bytes() == payload
    assert "installed" in run(FIXTURE_API_FAILURE="1").lower()
    assert "could not find" in run(ok=False, FIXTURE_API_FAILURE="1",
                                    FIXTURE_REDIRECT="https://github.com/other/luvus/releases/tag/fork-bad")
    assert installed.read_bytes() == payload
    if len(sys.argv) == 1:
        run(FIXTURE_OS="Darwin", FIXTURE_ARCH="arm64")
        run(FIXTURE_OS="Darwin", FIXTURE_ARCH="aarch64")
    checksum = root / "downloads/luvus-x86_64-unknown-linux-musl.tar.gz.sha256"
    checksum.write_text("0" * 64 + "  ignored-path\n")
    assert "SHA-256 mismatch" in run(ok=False)
    assert installed.read_bytes() == payload
    assert "unsupported" in run(ok=False, FIXTURE_ARCH="aarch64")
    assert "no fork binary" in run(ok=False, FIXTURE_OS="Windows_NT")
    assert "invalid release tag" in run(ok=False, LUVUS_VERSION="../../bad")
    assert "expected a fork release tag" in run(ok=False, LUVUS_VERSION="v0.13.4")
    # A missing checksum or failed network read must not replace the installation.
    checksum.unlink()
    assert "checksum download failed" in run(ok=False)
    assert installed.read_bytes() == payload
    archive = checksum.with_suffix("")
    checksum.write_text(hashlib.sha256(archive.read_bytes()).hexdigest() + "\n")
    original_archive = archive.read_bytes()
    # Even a correctly checksummed archive must prove the fork protocol before
    # the previous executable is replaced.
    bad_payload = b'#!/bin/sh\nprintf "luvus 0.13.4\\n"\n'
    with tarfile.open(archive, "w:gz") as tar:
        info = tarfile.TarInfo(f"{package}-x86_64-unknown-linux-musl/luvus")
        info.mode, info.size = 0o755, len(bad_payload)
        tar.addfile(info, io.BytesIO(bad_payload))
    checksum.write_text(hashlib.sha256(archive.read_bytes()).hexdigest() + "\n")
    assert "not a compatible modified" in run(ok=False)
    assert installed.read_bytes() == payload
    archive.write_bytes(original_archive)
    checksum.write_text(hashlib.sha256(original_archive).hexdigest() + "\n")
    if len(sys.argv) > 1:
        host_admission_smoke(repo, root, binary, env)
    installed.unlink()
    installed.symlink_to(root / "sentinel")
    (root / "sentinel").write_text("keep")
    assert "symlink" in run(ok=False)
    assert (root / "sentinel").read_text() == "keep"
    requests = (root / "requests").read_text()
    assert "RizRiyz" not in requests and "luvus.dev" not in requests
    print(f"PASS: fork installer, pinned update, backup, atomic rename, checksums, platform/tag/symlink rejection ({root})")


if __name__ == "__main__":
    if sys.argv[1:2] == ["--ssh-helper"]:
        raise SystemExit(ssh_helper())
    main()
