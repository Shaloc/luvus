#!/usr/bin/env python3
"""Unix smoke test: real Luvus servers/PTY clients with a local SSH substitute.

Run after cargo build: python3 scripts/test-remote-sessions.py
Use --restart-only for the isolated server restart --all smoke test.
Use --dimensions-only for the local split/close/resize regression.
Use --session-discovery-only for owner inventory and session deletion regression.
Use --remote-only-open-only for merge routing without implicit local owners.
All homes, sockets, files and child processes are isolated below target/.
No production server or actual SSH destination is accessed.
"""

import base64
import json
import os
from pathlib import Path
import re
import select
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
import unicodedata

IMAGE = base64.b64decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aN1sAAAAASUVORK5CYII=")
SELECTORS = ("LUVUS_SOCKET_PATH", "LUVUS_SESSION", "LUVUS_REMOTE_HOST", "LUVUS_REMOTE_SESSION",
             "LUVUS_PANE_ID", "LUVUS_API_ADDRESS", "LUVUS_SHELL")


def fixture_root():
    """Fail closed before a helper can connect, copy, or launch anything."""
    repo = Path(__file__).resolve().parent.parent
    root = Path(os.environ["LUVUS_SMOKE_ROOT"]).resolve()
    assert root.parent == repo / "target" and root.name.startswith("remote-smoke-"), root
    assert (root / ".isolated-smoke").read_text() == str(root), root
    binary = Path(os.environ["LUVUS_SMOKE_BINARY"]).resolve()
    assert binary.is_relative_to(repo / "target") and binary.is_file(), binary
    return root


def ssh_substitute():
    args = sys.argv[1:]
    while args and args[0].startswith("-"):
        args = args[2:] if args[0] == "-o" else args[1:]
    host, *command = args
    root = fixture_root()
    with (root / "ssh-calls").open("a") as log:
        log.write(host + "\n")
    if host == "fake-old":
        print("luvus 1.0.99")
        raise SystemExit(0)
    if host != "fake-dev":
        raise SystemExit("unexpected SSH destination: " + host)
    os.environ["HOME"] = str(root / "remote-home")
    os.environ["LUVUS_HOME"] = str(root / "remote-state")
    for key in SELECTORS:
        os.environ.pop(key, None)
    for key, suffix in (("XDG_CONFIG_HOME", "config"), ("XDG_DATA_HOME", "data"),
                        ("XDG_CACHE_HOME", "cache"), ("XDG_STATE_HOME", "state")):
        os.environ[key] = str(root / "remote-home" / suffix)
    assert command and Path(command[0]).name == "luvus", command
    os.execv(os.environ["LUVUS_SMOKE_BINARY"], command)


def main():
    repo = Path(__file__).resolve().parent.parent
    restart_only = "--restart-only" in sys.argv[1:]
    agent_state_only = "--agent-state-only" in sys.argv[1:]
    clipboard_only = "--clipboard-helper-only" in sys.argv[1:]
    dimensions_only = "--dimensions-only" in sys.argv[1:]
    session_discovery_only = "--session-discovery-only" in sys.argv[1:]
    remote_only_open_only = "--remote-only-open-only" in sys.argv[1:]
    positional = [arg for arg in sys.argv[1:] if arg not in ("--restart-only", "--agent-state-only", "--clipboard-helper-only", "--dimensions-only", "--session-discovery-only", "--remote-only-open-only")]
    binary = (Path(positional[0]) if positional else repo / "target/debug/luvus").resolve()
    if not binary.is_file():
        raise SystemExit("Build Luvus first: cargo build --locked")
    assert binary.is_relative_to(repo / "target"), "test only a checkout build, never an installed binary"
    root = Path(tempfile.mkdtemp(prefix="remote-smoke-", dir=repo / "target"))
    (root / ".isolated-smoke").write_text(str(root))
    clients = []
    env = {key: value for key, value in os.environ.items() if not key.startswith("LUVUS_")}
    for directory in ("local-home/.ssh", "local-home/picker-local", "local-state", "remote-home",
                      "remote-state", "bin", "project/picker-remote", "second"):
        (root / directory).mkdir(parents=True, exist_ok=True)
    git_env = dict(env, HOME=str(root / "local-home"), GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)
    for key in ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"):
        git_env.pop(key, None)
    subprocess.run(["git", "init", "--quiet", str(root / "project")], env=git_env, check=True)
    (root / "project/parity.md").write_text("# REMOTE_NATIVE_PREVIEW\n\noriginal parity line\n")
    subprocess.run(["git", "-C", str(root / "project"), "add", "parity.md"], env=git_env, check=True)
    subprocess.run(["git", "-C", str(root / "project"), "-c", "user.name=Smoke",
                    "-c", "user.email=smoke@example.invalid", "commit", "--quiet",
                    "--allow-empty", "-m", "fixture"], env=git_env, check=True)
    subprocess.run(["git", "-C", str(root / "project"), "worktree", "add", "--quiet",
                    "-b", "feature/smoke", str(root / "feature-checkout")], env=git_env, check=True)
    (root / "project/parity.md").write_text("# REMOTE_NATIVE_PREVIEW\n\nmodified parity line\n")
    # A symlink invokes this same file's narrow SSH-substitute role.
    (root / "bin/ssh").symlink_to(Path(__file__).resolve())
    (root / "local-home/.ssh/config").write_text("Host fake-dev fake-old unselected\n  HostName 192.0.2.1\n")
    # All native copy helpers are private sinks. OSC 52 is observed only in
    # this script's PTY pipe, never printed to the operator's real terminal.
    for helper in ("xclip", "xsel", "wl-copy", "pbcopy"):
        (root / "bin" / helper).symlink_to(Path(__file__).resolve())
    for state in ("local-state", "remote-state"):
        (root / state / "config.json").write_text(json.dumps({
            "check_updates": False, "remote_hosts": [], "shell": "/bin/sh",
            "prefix": "ctrl+b" if state == "local-state" else "ctrl+space",
        }))
    env.update(HOME=str(root / "local-home"), LUVUS_HOME=str(root / "local-state"),
               PATH=str(root / "bin") + ":" + env.get("PATH", ""), TERM="xterm-256color",
               LUVUS_SMOKE_ROOT=str(root), LUVUS_SMOKE_BINARY=str(binary), DISPLAY=":smoke")
    env.pop("WAYLAND_DISPLAY", None)
    if clipboard_only:
        helper = Path(os.environ["LUVUS_TEST_KITTEN"]).resolve()
        assert helper.is_relative_to(repo / "target") and helper.is_file(), helper
        shutil.copyfile(helper, root / "official-kitten")
        (root / "bin/curl").symlink_to(Path(__file__).resolve())
        env["TERM"] = "xterm-kitty"
        env.pop("DISPLAY", None)
    for key, suffix in (("XDG_CONFIG_HOME", "config"), ("XDG_DATA_HOME", "data"),
                        ("XDG_CACHE_HOME", "cache"), ("XDG_STATE_HOME", "state")):
        env[key] = str(root / "local-home" / suffix)
    remote_env = dict(env, HOME=str(root / "remote-home"), LUVUS_HOME=str(root / "remote-state"))
    for key in ("XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME"):
        remote_env[key] = remote_env[key].replace("local-home", "remote-home")

    def assert_isolated(selected_env):
        assert root.parent == repo / "target" and (root / ".isolated-smoke").read_text() == str(root)
        assert selected_env["LUVUS_HOME"] in (str(root / "local-state"), str(root / "remote-state"))
        assert selected_env["HOME"] in (str(root / "local-home"), str(root / "remote-home"))
        assert Path(selected_env["PATH"].split(os.pathsep)[0]) == root / "bin"
        for key in ("LUVUS_SOCKET_PATH", "LUVUS_API_ADDRESS"):
            if selected_env.get(key):
                assert Path(selected_env[key]).resolve().is_relative_to(root), (key, selected_env[key])

    def run(*args, remote=False, input_text=None, okay=True):
        assert_isolated(remote_env if remote else env)
        result = subprocess.run([str(binary), *args], env=remote_env if remote else env,
                                cwd=root / "project", input=input_text, text=True,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=25)
        if okay and result.returncode:
            raise AssertionError(f"{args}: {result.stdout}\n{result.stderr}")
        return result

    def api(method, params=None, remote=False, session="api"):
        response = run("--session", session, "uhp", "proxy", remote=remote,
                       input_text=json.dumps({"id": "smoke", "method": method, "params": params or {}}) + "\n")
        value = json.loads(response.stdout)
        assert "result" in value, (method, params, value)
        return value["result"]

    def host_cli(*args):
        value = json.loads(run("--host", "fake-dev", "--session", "api", *args).stdout)
        assert "error" not in value, (args, value)
        return value.get("result", value)

    def case(label, check):
        try:
            check()
        except Exception:
            print("FAIL:", label, file=sys.stderr, flush=True)
            raise
        print("PASS:", label, flush=True)

    def wait_for(check):
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            value = check()
            if value:
                return value
            time.sleep(0.08)
        raise AssertionError("condition did not become true")

    def projected():
        return [w for w in api("session.snapshot")["workspaces"] if w.get("host") == "fake-dev"]

    def restart_smoke():
        names = ("default", "api", "second")

        def generations(remote=False):
            return {name: api("uhp.capabilities", remote=remote, session=name)["server_generation"]
                    for name in names}

        empty = json.loads(run("server", "restart", "--all", "--json").stdout)
        assert empty["sessions"] == [] and empty["restarted"] == 0 and empty["failed"] == 0
        assert not (root / "ssh-calls").exists()
        assert run("skill", "show").stdout.strip() == (repo / "skills/luvus/SKILL.md").read_text().strip()
        for remote in (False, True):
            for name in (*names, "saved"):
                run("--session", name, "server", "start", remote=remote)
            run("--session", "saved", "server", "stop", remote=remote)
        local_before, remote_before = generations(), generations(remote=True)
        saved = root / "local-state/sessions/saved/session.json"
        saved_before = saved.read_bytes()

        # Invalid flags must be rejected before any server is stopped.
        for flags in (("--all", "--unknown"), ("--all", "--all"), ("--json",),
                      ("-all",), ("-all", "--json")):
            assert run("server", "restart", *flags, okay=False).returncode
        assert generations() == local_before

        # An explicit selected session does not limit --all to that namespace.
        batch = json.loads(run("--session", "api", "server", "restart", "--all", "--json").stdout)
        assert batch["restarted"] == 3 and batch["failed"] == 0, batch
        assert {row["session"] for row in batch["sessions"]} == set(names), batch
        assert batch["skipped"] == ["saved"], batch
        assert all(generations()[name] != local_before[name] for name in names)
        assert generations(remote=True) == remote_before
        assert saved.read_bytes() == saved_before
        inventory = json.loads(run("remote-session-list").stdout)
        assert not next(row for row in inventory if row["name"] == "saved")["running"]

        # Bare restart remains one-session-only.
        before = generations()
        run("--session", "api", "server", "restart")
        after = generations()
        assert after["api"] != before["api"]
        assert after["default"] == before["default"] and after["second"] == before["second"]

        # The same flags go through the existing SSH lifecycle command, and a
        # local batch with an enabled host must still leave remote owners alone.
        api("config.patch", {"patch": {"remote_hosts": ["fake-dev"]}})
        before = generations()
        assert run("--host", "fake-dev", "server", "restart", "-all", okay=False).returncode
        assert generations(remote=True) == remote_before
        batch = json.loads(run("--host", "fake-dev", "server", "restart", "--all", "--json").stdout)
        assert batch["restarted"] == 3 and batch["skipped"] == ["saved"], batch
        assert all(generations(remote=True)[name] != remote_before[name] for name in names)
        assert generations() == before
        remote_before = generations(remote=True)
        # Even a stale inherited pane selector cannot send a local batch to
        # another home or limit it to a stopped namespace.
        env["LUVUS_SOCKET_PATH"] = str(root / "remote-state/luvus.sock")
        env["LUVUS_SESSION"] = "saved"
        try:
            human = run("server", "restart", "--all").stdout
        finally:
            env.pop("LUVUS_SOCKET_PATH")
            env.pop("LUVUS_SESSION")
        assert all(name + ":" in human for name in names), human
        assert generations(remote=True) == remote_before
        print(human.strip())
        print("PASS: --all restarts all running sessions; saved sessions stay stopped; "
              "plain restart stays scoped; invalid flags are non-mutating; --host isolates the selected machine")

    try:
        if restart_only:
            restart_smoke()
            return
        absent = run("--session", "search-stopped", "remote-control-bridge", "--existing",
                     input_text='{"id":"probe","method":"ping"}\n', okay=False)
        assert absent.returncode, "existing-only discovery must fail for an absent owner"
        assert not (root / "local-state/sessions/search-stopped/server.pid").exists()
        print("PASS: existing-only search bridge does not start an absent owner", flush=True)
        denied = run("--host", "unselected", "pane", "list", okay=False)
        assert denied.returncode and "Settings > Remote" in denied.stderr
        assert not (root / "ssh-calls").exists(), "disabled hosts must be rejected before SSH"
        run("--session", "api", "server", "start", remote=True)
        run("--session", "second", "server", "start", remote=True)
        run("--session", "api", "server", "start")
        for remote in (False, True):
            assert api("config.get", remote=remote)["config"]["layout"]["auto_workspace_rehome"] is False
        api("config.patch", {"patch": {"layout": {"auto_workspace_rehome": True}}})
        assert api("config.get")["config"]["layout"]["auto_workspace_rehome"] is True
        assert api("config.get", remote=True)["config"]["layout"]["auto_workspace_rehome"] is False
        api("config.patch", {"patch": {"layout": {"auto_workspace_rehome": False}}})
        print("PASS: automatic workspace rehome defaults off on both owners; live settings stay owner-local", flush=True)
        api("config.patch", {"patch": {"remote_hosts": ["fake-dev", "fake-old"]}})
        missing = run("--host", "fake-dev", "--session", "search-stopped", "workspace", "list", okay=False)
        assert missing.returncode, "remote CLI must not start a stopped owner"
        absent_frame = run("--session", "search-stopped", "remote-client-bridge", "--existing",
                           remote=True, okay=False)
        assert absent_frame.returncode, "frame subscription must not start an owner"
        assert not (root / "remote-state/sessions/search-stopped/server.pid").exists()
        print("PASS: remote CLI and frame discovery never start a stopped owner", flush=True)
        wait_for(lambda: (root / "ssh-calls").exists())
        discovered = json.loads(run("session", "remote", "list", "--json").stdout)
        assert {s["session"] for s in discovered["sessions"]} >= {"api", "second"}
        assert any(h["host"] == "fake-old" and h["error"] for h in discovered["hosts"])
        run("--session", "remote-fake-dev-second", "ping")
        # Owner names starting with remote- must not be parsed as another hop.
        run("session", "remote", "add", "fake-dev", "remote-nested")
        run("--host", "fake-dev", "--session", "remote-nested", "ping")
        run("--host", "fake-dev", "--session", "remote-nested", "server", "status")
        run("--session", "remote-fake-dev-remote-nested", "ping")
        rejected = run("session", "remote", "add", "fake-old", "api", okay=False)
        assert rejected.returncode and "modified remote-session build" in rejected.stderr

        run("session", "remote", "add", "fake-dev", "api", "--merge")
        wait_for(projected)
        # The actual presentation server writes its own identity at startup;
        # public inventories must not re-export it as an owner, even after stop.
        proxy = "remote-fake-dev-api"
        owner_generation = api("uhp.capabilities", remote=True)["server_generation"]
        api("session.start", {"name": proxy})
        marker = root / "local-state/sessions" / proxy / "session-origin.json"
        assert json.loads(marker.read_text()) == {
            "kind": "remote_view", "target": {"host": "fake-dev", "session": "api"}}
        assert api("session.status", {"name": proxy})["session"]["running"]
        for stopped in (False, True):
            if stopped:
                run("--session", proxy, "remote-server-command", "stop")
            assert proxy not in {s["name"] for s in json.loads(run("remote-session-list").stdout)}
            assert proxy not in {s["name"] for s in api("session.list")["sessions"]}
            owners = api("session.list")["sessions"]
            assert api("host.info")["sessions"] == {"total": len(owners), "running": sum(s["running"] for s in owners)}
            assert proxy not in {s["name"] for s in json.loads(run("session", "list", "--json").stdout)["sessions"]}
            assert marker.exists(), "stopping must retain the presentation identity"
        literal = "remote-fake-dev-literal"
        run("--session", literal, "remote-server-command", "start")
        assert json.loads((root / "local-state/sessions" / literal / "session-origin.json").read_text())["kind"] == "local"
        assert literal in {s["name"] for s in json.loads(run("remote-session-list").stdout)}
        assert run("session", "delete", literal, okay=False).returncode, "running owner deletion must fail"
        run("--session", literal, "remote-server-command", "stop")
        assert not next(s for s in json.loads(run("remote-session-list").stdout) if s["name"] == literal)["running"]
        run("session", "delete", literal)
        assert not (root / "local-state/sessions" / literal).exists()
        assert api("uhp.capabilities", remote=True)["server_generation"] == owner_generation
        print("PASS: presentation startup/stop excluded from public inventories; literal remote- owner survives discovery; delete rejects running owner", flush=True)
        run("--session", "remote-fake-dev-api", "workspace", "list")
        run("--host", "fake-dev", "--session", "api", "pane", "list")
        worktrees = run("--session", "api", "worktree", "list", "--host", "fake-dev")
        assert "feature/smoke" in worktrees.stdout and "feature-checkout" in worktrees.stdout
        pane = api("pane.list", remote=True)["panes"][0]["pane"]
        run("--host", "fake-dev", "--session", "api", "pane", "run", str(pane), "printf REMOTE_SMOKE_MARKER")
        wait_for(lambda: "REMOTE_SMOKE_MARKER" in json.dumps(api("pane.read", {"pane": pane}, remote=True)))

        # Standalone attach is deliberately tested with global merge off. When
        # merge is on, managed attach correctly enters the same-name local UI.
        run("session", "merge", "off")
        wait_for(lambda: not projected())

        # Real thin clients perform both local and managed remote handshakes.
        import fcntl
        import pty
        import struct
        import termios
        def start_client(args):
            assert_isolated(env)
            master, slave = pty.openpty()
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 120, 0, 0))
            process = subprocess.Popen([str(binary), *args], env=env, cwd=root / "project",
                                       stdin=slave, stdout=slave, stderr=slave, start_new_session=True,
                                       preexec_fn=lambda: fcntl.ioctl(0, termios.TIOCSCTTY, 0))
            os.close(slave)
            clients.append((process, master))
            screen = bytearray()
            deadline = time.monotonic() + 4
            while time.monotonic() < deadline and b"luvus" not in screen.lower():
                if select.select([master], [], [], 0.1)[0]:
                    screen.extend(os.read(master, 65536))
            assert process.poll() is None and b"luvus" in screen.lower(), screen[-1000:]
            assert b"\x1b[?1049h" in screen, "the fixture must observe the initial alternate-screen entry"
            return process, master

        def drain(master, duration=0.5):
            screen = bytearray()
            deadline = time.monotonic() + duration
            while time.monotonic() < deadline:
                if select.select([master], [], [], 0.05)[0]:
                    screen.extend(os.read(master, 65536))
            return screen

        def repaint(master):
            # A viewport change forces a complete owner frame, making the
            # clickable labels observable without assuming fixed dock geometry.
            fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 121, 0, 0))
            drain(master, 0.3)
            fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 120, 0, 0))
            return drain(master, 0.7)

        def position(screen, label, before_column=None, after_column=0, after_row=0, before_row=None):
            text = screen.decode("utf-8", errors="replace")
            text = re.sub(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)", "", text)
            text = re.sub(r"\x1b\[[0-9;?]*[A-GI-Za-z]", "", text)
            # A remote resize produces an immediate outer frame followed by
            # the resized owner frame. Select the last rendered occurrence,
            # never the old menu position from the earlier frame in this read.
            for move in reversed(list(re.finditer(r"\x1b\[(\d+);(\d+)H([^\x1b]*)", text))):
                chunk = move.group(3)
                # Distinguish actual controls from Ctrl+B and terminal markers
                # such as MENU_NAV_*. The latter must never become UI targets.
                pattern = (r"(?<!\S)\+(?!\S)" if label == "+" else
                           r"(?<!\w)MENU(?!\w)" if label == "MENU" else re.escape(label))
                found = re.search(pattern, chunk)
                if found is None:
                    continue
                prefix = chunk[:found.start()]
                width = sum(0 if unicodedata.combining(char) else
                            2 if unicodedata.east_asian_width(char) in ("W", "F") else 1
                            for char in prefix)
                x, y = int(move.group(2)) + width, int(move.group(1))
                if (x >= after_column and (before_column is None or x < before_column)
                        and y >= after_row and (before_row is None or y < before_row)):
                    return x, y
            raise AssertionError(f"clickable label {label!r} not rendered: {text[-7000:]}")

        # Validate the click-target reader itself against mixed resize frames
        # and lookalike terminal text before using it as regression evidence.
        assert position(b"\x1b[28;104HOwner proof\x1b[32;116HOwner proof", "Owner proof") == (116, 32)
        assert position(b"\x1b[3;22H + \x1b[30;2HCtrl+B", "+") == (23, 3)
        assert position(b"\x1b[1;28HMENU\x1b[20;28HMENU_NAV_source", "MENU") == (28, 1)

        def click(master, x, y, button=0):
            os.write(master, f"\x1b[<{button};{x};{y}M\x1b[<{button};{x};{y}m".encode())

        if remote_only_open_only:
            run("session", "merge", "on")
            # A legitimate local owner may have the presentation's internal name.
            # Opening a remote must neither attach to it nor overwrite saved state.
            collision = "remote-fake-dev-second"
            collision_dir = root / "local-state/sessions" / collision
            run("--session", collision, "remote-server-command", "start")
            for running in (True, False):
                if not running:
                    run("--session", collision, "remote-server-command", "stop")
                before = {path.name: path.read_bytes() for path in collision_dir.iterdir()
                          if path.name in ("session-origin.json", "session.json")}
                rejected = run("session", "attach", collision, okay=False)
                assert rejected.returncode != 0 and "existing local session" in rejected.stderr, rejected
                after = {path.name: path.read_bytes() for path in collision_dir.iterdir()
                         if path.name in ("session-origin.json", "session.json")}
                assert before == after, "remote attach changed a real owner's identity or snapshot"
                assert api("session.status", {"name": collision})["session"]["running"] == running
            assert "session.json" in before, "collision fixture must have saved state to protect"
            run("session", "delete", collision)
            print("PASS: remote view collisions reject running/stopped local owners and preserve saved layout", flush=True)
            def no_local_owner(name):
                if name == "default":
                    for filename in ("session-origin.json", "session.json", "server.pid"):
                        assert not (root / "local-state" / filename).exists(), "remote-only default created a local owner"
                else:
                    assert not (root / "local-state/sessions" / name).exists(), f"remote-only {name} created a local owner"

            for name in ("second", "default"):
                no_local_owner(name)
                process, master = start_client(["session", "attach", "remote-fake-dev-" + name])
                repaint(master)
                no_local_owner(name)
                assert api("ping", remote=True, session=name)["session"] == name
                process.terminate()
                process.wait(timeout=5)
                os.close(master)
                clients.pop()
            print("PASS: merge-on CLI attach opens remote-only named/default owners without creating local copies", flush=True)

            process, master = start_client(["--session", "api"])
            repaint(master)
            switched = bytearray()
            def remote_opened(name):
                switched.extend(drain(master, 0.1))
                no_local_owner(name)
                return ("luvus · " + name + " [remote · fake-dev]").encode() in switched
            for name in ("default", "second"):
                if name == "default":
                    run("session", "merge", "off")
                os.write(master, b"\x02t")
                drain(master, 1.5)
                if name == "default":
                    # A setting reload while the selector is open must not
                    # turn a synthetic local entry into creation authority.
                    run("session", "merge", "on")
                    drain(master)
                screen = repaint(master)
                click(master, *position(screen, name))
                switched.clear()
                wait_for(lambda: remote_opened(name))
                assert b"\x1b[?1049l" not in switched
            for enabled in ("off", "on"):
                run("session", "merge", enabled)
                repaint(master)
                no_local_owner("second")
            os.write(master, b"\x02t")
            drain(master, 1.5)
            os.write(master, b"\x1b[Hj\r")
            drain(master)
            os.write(master, b"only-created\r")
            switched.clear()
            wait_for(lambda: remote_opened("only-created"))
            no_local_owner("only-created")
            assert api("ping", remote=True, session="only-created")["session"] == "only-created"
            assert process.poll() is None
            print("PASS: merge-on switcher/New Remote and off/on toggle keep remote-only owners remote; thin client stays attached", flush=True)
            return

        if session_discovery_only:
            saved = "remote-saved-review"
            def seed(remote):
                run("--session", saved, "remote-server-command", "start", remote=remote)
                run("--session", saved, "remote-server-command", "stop", remote=remote)

            def saved_dir(remote):
                return root / ("remote-state" if remote else "local-state") / "sessions" / saved

            for remote in (False, True):
                seed(remote)
            # Test the local and remote copies of the same merged row. Only
            # the fixture's owner namespaces may be deleted, never the project.
            run("session", "merge", "on")
            process, master = start_client(["--session", "api"])
            repaint(master)

            def open_delete(host):
                os.write(master, b"\x02t")
                drain(master, 1.5)
                screen = repaint(master)
                click(master, *position(screen, saved), button=2)
                screen = repaint(master)
                label = "Delete · " + host + " · " + saved
                click(master, *position(screen, label))
                screen = repaint(master)
                position(screen, "Delete saved session and logs?")
                return label, screen

            label, screen = open_delete("Local machine")
            # Keyboard confirmation defaults to Cancel, without any deletion.
            os.write(master, b"\r")
            drain(master)
            assert saved_dir(False).exists() and saved_dir(True).exists()
            os.write(master, b"\x1b")
            drain(master)
            label, screen = open_delete("Local machine")
            click(master, *position(screen, label))
            wait_for(lambda: not saved_dir(False).exists())
            assert saved_dir(True).exists()
            drain(master, 1.5)
            os.write(master, b"\x1b")
            drain(master)
            print("PASS: merged stopped row: right-click Delete names the local owner; default Cancel preserves both; confirmation removes local only", flush=True)

            seed(False)
            label, screen = open_delete("fake-dev")
            # Real owner-side revalidation: the stale confirmation is not
            # authority to stop a session that started in the meantime.
            run("--session", saved, "remote-server-command", "start", remote=True)
            click(master, *position(screen, label))
            failure = bytearray()
            def refused():
                failure.extend(drain(master, 0.1))
                # The bounded error row may truncate the trailing CLI advice.
                return b"could not delete session" in failure
            wait_for(refused)
            assert api("session.status", {"name": saved}, remote=True)["session"]["running"]
            assert saved_dir(False).exists() and saved_dir(True).exists()
            run("--session", saved, "remote-server-command", "stop", remote=True)
            os.write(master, b"\x1b")
            drain(master)
            label, screen = open_delete("fake-dev")
            click(master, *position(screen, label))
            wait_for(lambda: not saved_dir(True).exists())
            assert saved_dir(False).exists()
            assert (root / "project/parity.md").is_file()
            assert api("uhp.capabilities", remote=True)["server_generation"] == owner_generation
            assert process.poll() is None
            print("PASS: merged remote Delete refuses a restarted owner; retry deletes exact literal remote- name; local copy, project files and active remote owner survive", flush=True)

            drain(master, 1.5)
            os.write(master, b"\x1b")
            drain(master)
            seed(True)
            run("session", "merge", "off")
            label, screen = open_delete("fake-dev")
            # Click outside the confirmation over the terminal: dismiss only,
            # without confirming or forwarding the click to the remote pane.
            click(master, 119, 28)
            drain(master)
            assert saved_dir(False).exists() and saved_dir(True).exists()
            os.write(master, b"\x1b")
            drain(master)
            label, screen = open_delete("fake-dev")
            click(master, *position(screen, label))
            wait_for(lambda: not saved_dir(True).exists())
            assert saved_dir(False).exists()
            assert api("uhp.capabilities", remote=True)["server_generation"] == owner_generation
            print("PASS: unmerged remote stopped row: outside click cancels; confirmed Delete removes remote only", flush=True)
            return

        if agent_state_only:
            run("session", "merge", "on")
            wait_for(projected)
            process, master = start_client(["--session", "api"])
            drain(master)
            command = "exec -a qodercli " + shlex.join([sys.executable, str(Path(__file__).resolve()), "--qoder-fixture"])
            run("--host", "fake-dev", "--session", "api", "pane", "run", pane,
                "bash -c " + shlex.quote(command))
            # Native Qoder screen evidence, not agent.report. No local click,
            # resize or API mutation may be needed to refresh a remote row.
            for expected, text in (("idle", "Ready for your next task"),
                                   ("working", "Generating answer (esc to cancel, 12s)"),
                                   ("blocked", "Permission required for shell"),
                                   ("working", "Generating answer (esc to cancel, 13s)"),
                                   ("idle", "Ready for your next task")):
                started = time.monotonic()
                run("--host", "fake-dev", "--session", "api", "pane", "run", pane,
                    text)
                wait_for(lambda: any(a["pane"] == pane and a["agent"] == "qodercli"
                                     and a["status"] == expected
                                     for a in api("agent.list", remote=True)["agents"]))
                screen = bytearray()
                def synchronized():
                    screen.extend(drain(master, 0.1))
                    return any(a.get("owner_pane") == pane and a["status"] == expected
                               for a in api("agent.list")["agents"])
                wait_for(synchronized)
                screen.extend(drain(master, 0.3))
                position(screen, expected, before_column=40)
                assert process.poll() is None
                print("PASS: native Qoder state without navigation:", expected,
                      f"{time.monotonic() - started:.3f}s", flush=True)
            return

        if clipboard_only:
            clipboard_mode = os.environ.get("LUVUS_TEST_CLIPBOARD_MODE", "local")
            assert clipboard_mode in ("local", "direct-remote", "merge"), clipboard_mode
            if clipboard_mode == "merge":
                run("session", "merge", "on")
                wait_for(projected)
                remote_workspace = next(w for w in api("workspace.list")["workspaces"] if w.get("host") == "fake-dev")
                api("workspace.focus", {"workspace": remote_workspace["workspace"]})
            process, master = start_client(["session", "attach", "remote-fake-dev-api"]
                                           if clipboard_mode == "direct-remote" else ["--session", "api"])
            os.write(master, b"\x02=")
            screen = bytearray()
            def install_row():
                screen.extend(drain(master, 0.1))
                return b"Install" in screen and b"Kitty clipboard" in screen
            wait_for(install_row)
            assert not (root / "curl-calls").exists(), "opening General must not download"
            # A corrupt download must not become executable, and the same
            # clickable row must permit a retry with the verified official file.
            (root / "bad-download").touch()
            click(master, *position(screen, "Kitty clipboard"))
            failure = bytearray()
            def failed():
                failure.extend(drain(master, 0.1))
                return b"SHA-256 mismatch" in failure
            wait_for(failed)
            installed = root / "local-state/tools/kitten"
            assert not installed.exists()
            (root / "bad-download").unlink()
            click(master, *position(screen, "Kitty clipboard"))
            success = bytearray()
            def completed():
                success.extend(drain(master, 0.1))
                return b"kitten 0.48.2" in success
            wait_for(completed)
            assert installed.read_bytes() == (root / "official-kitten").read_bytes()
            assert installed.stat().st_mode & 0o777 == 0o700
            assert not (root / "remote-state/tools/kitten").exists()
            print(f"PASS: {clipboard_mode}: General click installs verified official kitten on display only; corrupt download and retry", flush=True)
            os.write(master, b"\x1b")
            drain(master)
            os.write(master, b"\x16")
            clipboard_wire = bytearray()
            requests = []
            def image_staged():
                clipboard_wire.extend(drain(master, 0.05))
                if b"\x1b[?5522$p" in clipboard_wire:
                    clipboard_wire[:] = clipboard_wire.replace(b"\x1b[?5522$p", b"")
                    os.write(master, b"\x1b[?5522;2$y")
                for match in list(re.finditer(rb"\x1b\]5522;([^;\x1b]*);([^\x1b]*)\x1b\\", clipboard_wire)):
                    metadata, payload = match.groups()
                    if b"type=read" not in metadata:
                        continue
                    mime = base64.b64decode(payload)
                    requests.append(mime)
                    content = b"image/png" if mime == b"." else IMAGE
                    kind = b"." if mime == b"." else b"image/png"
                    response = (b"\x1b]5522;type=read:status=OK\x1b\\"
                                b"\x1b]5522;type=read:status=DATA:mime=" + base64.b64encode(kind)
                                + b";" + base64.b64encode(content) + b"\x1b\\"
                                b"\x1b]5522;type=read:status=DONE\x1b\\")
                    os.write(master, response)
                if requests:
                    clipboard_wire.clear()
                owner_state = "local-state" if clipboard_mode == "local" else "remote-state"
                images = list((root / owner_state / "sessions/api/clipboard").glob("*.png"))
                return bool(images) and images[0].read_bytes() == IMAGE
            try:
                wait_for(image_staged)
            except AssertionError as error:
                raise AssertionError(f"official kitten request={requests!r}, wire={clipboard_wire!r}") from error
            assert process.poll() is None
            print(f"PASS: {clipboard_mode}: Ctrl+V through the real official kitten and an isolated OSC 5522 terminal peer stages exact PNG bytes", flush=True)
            return

        def topology(remote=False):
            # Ignore focus, revisions and detection timing; preserve everything
            # that could reveal a command mutating the wrong owner's layout.
            return [{"id": w["id"], "name": w["name"], "cwd": w["cwd"],
                     "tabs": [{"name": t["name"], "kind": t["kind"],
                               "panes": [(p["pane_id"], p["kind"]) for p in t["panes"]]}
                              for t in w["tabs"]]}
                    for w in api("session.snapshot", remote=remote)["workspaces"] if not w.get("host")]

        parity_dimensions = {}

        def owner_matrix(master, mode):
            """Identical interaction chain for local, direct remote and merge."""
            remote = mode != "local"
            untouched = topology(remote=not remote)

            def owner(method, params=None):
                return api(method, params, remote=remote)

            def cli(*args):
                if remote:
                    return host_cli(*args)
                value = json.loads(run("--session", "api", *args).stdout)
                assert "error" not in value, (args, value)
                return value.get("result", value)

            def unchanged():
                assert topology(remote=not remote) == untouched, "the other owner's topology changed"

            def row(name, check):
                def isolated_check():
                    check()
                    unchanged()
                case(f"{mode}: {name}", isolated_check)

            original_tabs = owner("tab.list")["tabs"]
            original_active = next(t["tab"] for t in original_tabs if t["active"])
            temp_tab = None
            temp_pane = None

            def tabs():
                nonlocal temp_tab, temp_pane
                screen = repaint(master)
                click(master, *position(screen, "+", after_column=35))
                current = wait_for(lambda: owner("tab.list")["tabs"]
                                   if len(owner("tab.list")["tabs"]) == len(original_tabs) + 1 else None)
                temp_tab = next(t["tab"] for t in current if t["active"])
                cli("tab", "rename", "parity-tab", "--tab", temp_tab)
                assert next(t for t in owner("tab.list")["tabs"] if t["tab"] == temp_tab)["name"] == "parity-tab"
                cli("tab", "focus", original_active)
                screen = repaint(master)
                click(master, *position(screen, "parity-tab", after_column=35))
                wait_for(lambda: any(t["tab"] == temp_tab and t["active"] for t in owner("tab.list")["tabs"]))
                temp_pane = next(p["pane"] for p in owner("pane.list")["panes"] if p["focused"])

            row("tab-row + creates owner tab; rename and mouse focus", tabs)

            def panes():
                before = owner("pane.layout", {"pane": temp_pane})["rect"]
                split = cli("pane", "split", str(temp_pane))["pane"]
                assert split != temp_pane
                cli("pane", "focus", str(temp_pane))
                wait_for(lambda: any(p["pane"] == temp_pane and p["focused"] for p in owner("pane.list")["panes"]))
                divided = owner("pane.layout", {"pane": temp_pane})["rect"]
                assert divided["width"] < before["width"]
                owner("pane.resize", {"pane": temp_pane, "direction": "right", "cells": 3})
                assert owner("pane.layout", {"pane": temp_pane})["rect"] != divided
                cli("pane", "close", str(split))
                wait_for(lambda: not any(p["pane"] == split for p in owner("pane.list")["panes"]))
                assert owner("pane.layout", {"pane": temp_pane})["rect"] == before

            row("pane split/focus/resize/close stays on owner", panes)

            def dimensions():
                def size(label):
                    marker = f"PTY_SIZE_{mode.replace('-', '_')}_{label}"
                    cli("pane", "run", str(temp_pane),
                        f"set -- $(stty size); printf '\\n{marker}_%s_%s\\n' \"$1\" \"$2\"")
                    pattern = re.compile(rf"(?:^|\n){marker}_(\d+)_(\d+)(?:\r?\n|$)")
                    match = wait_for(lambda: pattern.search(owner("pane.read", {"pane": temp_pane})["text"]))
                    return tuple(map(int, match.groups()))
                # Consume the display after the split/close API sequence.
                # Those calls confirm logical topology, not a rendered frame.
                # Let the display consume pending frames and the asynchronous
                # PTY resize settle before sampling the unsplit baseline.
                drain(master)
                before = size("before")
                assert 10 <= before[0] <= 30 and 40 <= before[1] <= 120, before
                if mode != "local":
                    assert before == parity_dimensions["local"], (mode, before, parity_dimensions["local"])
                parity_dimensions[mode] = before
                fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 34, 132, 0, 0))
                drain(master, 0.7)
                larger = size("larger")
                assert larger == (before[0] + 4, before[1] + 12), (before, larger)
                fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 120, 0, 0))
                drain(master, 0.7)
                assert size("restored") == before

            row("display resize reaches actual owner PTY; grow and restore", dimensions)
            if dimensions_only:
                return

            def prefix_help():
                drain(master)
                os.write(master, b"\x02?")
                help_screen = drain(master, 0.7)
                assert b"shortcuts" in help_screen.lower(), help_screen[-5000:]
                expected_prefix = b"Ctrl+Space" if remote else b"Ctrl+B"
                assert expected_prefix.lower() in help_screen.lower(), help_screen[-5000:]
                os.write(master, b"\x1b")
                drain(master)
                # Fixed tab digits use the same owner prefix handler as '?'.
                os.write(master, b"\x02" + str(original_active).encode())
                wait_for(lambda: any(t["tab"] == original_active and t["active"] for t in owner("tab.list")["tabs"]))
                cli("tab", "focus", temp_tab)

            row("prefix help and fixed tab digits execute on owner despite different prefixes", prefix_help)

            def clipboard():
                payload = f"OSC52 {mode} indentation\n  second line 中文".encode()
                encoded = base64.b64encode(payload)
                drain(master)
                cli("pane", "run", str(temp_pane),
                    "printf '\\033]52;c;" + encoded.decode() + "\\007'")
                output = bytearray()
                def copied():
                    output.extend(drain(master, 0.1))
                    return any(base64.b64decode(value) == payload for value in
                               re.findall(rb"\x1b\]52;c;([A-Za-z0-9+/=]*)(?:\x07|\x1b\\)", output))
                wait_for(copied)
                # Native fallback must also have landed in our private helper,
                # never in an installed clipboard command or a real display.
                assert (root / "clipboard-copy").read_bytes() == payload

            row("child OSC52 copy forwards exact bytes to display client", clipboard)

            if shutil.which("nvim", path=env["PATH"]):
                def nvim_yank():
                    payload = f"NVIM_YANK_{mode} 中文".encode()
                    expected = payload + b"\n"  # yy is a linewise yank.
                    setup = "lua vim.api.nvim_buf_set_lines(0, 0, -1, false, {" + json.dumps(payload.decode(), ensure_ascii=False) + "})"
                    command = shlex.join(["nvim", "-u", "NONE", "-i", "NONE", "--noplugin",
                                          "--cmd", "let g:clipboard='osc52'", "-c", setup,
                                          "-c", 'normal! gg"+yy', "-c", "sleep 20m", "-c", "qa!"])
                    drain(master)
                    cli("pane", "run", str(temp_pane), command)
                    output = bytearray()
                    def copied():
                        output.extend(drain(master, 0.1))
                        return any(base64.b64decode(value) == expected for value in
                                   re.findall(rb"\x1b\]52;c;([A-Za-z0-9+/=]*)(?:\x07|\x1b\\)", output))
                    wait_for(copied)
                    assert (root / "clipboard-copy").read_bytes() == expected
                row("real Neovim OSC52 + register yank reaches display clipboard", nvim_yank)
            else:
                print(f"SKIP: {mode}: Neovim executable is unavailable", flush=True)

            def native_views():
                cli("files", "refresh")
                tree = wait_for(lambda: cli("files", "tree")
                                if any(r["name"] == "parity.md" for r in cli("files", "tree")["rows"]) else None)
                assert tree["root"] == str(root / "project")
                cli("files", "open", "parity.md", "--target", "tab")
                def native():
                    return any(p["kind"] == "view" for w in owner("session.snapshot")["workspaces"]
                               for t in w["tabs"] if t["active"] for p in t["panes"])
                wait_for(native)
                assert b"REMOTE_NATIVE_PREVIEW" in repaint(master), "owner Markdown view content missing"
                cli("tab", "close")
                cli("tab", "focus", temp_tab)
                diff = cli("diff", "get", "parity.md", "--include-patch")
                assert "modified parity line" in json.dumps(diff), diff
                cli("diff", "open", "parity.md", "--placement", "tab")
                wait_for(native)
                assert b"modified parity line" in repaint(master), "owner DIFF content missing"
                cli("tab", "close")
                cli("tab", "focus", temp_tab)
                assert "parity.md" in json.dumps(cli("git", "status"))
                assert cli("git", "branches")["branches"]
                cli("git", "open")
                wait_for(lambda: any(t["kind"] == "git" and t["active"] for t in owner("tab.list")["tabs"]))
                cli("tab", "close")
                cli("tab", "focus", temp_tab)

            row("FILES tree, native Markdown, DIFF patch/view and Git dashboard", native_views)

            def search():
                result = cli("search", "--fuzzy", "parity.md", "--scope", "files")
                assert result["matches"] and "parity.md" in json.dumps(result["matches"]), result

            row("fuzzy file-search CLI resolves owner fixture", search)

            def worktrees():
                branch = "parity-" + mode
                made = cli("worktree", "create", branch)
                path = Path(made["path"])
                assert path.is_relative_to(root / ("remote-state" if remote else "local-state")), path
                assert path.is_dir()
                assert any(w["path"] == str(path) for w in cli("worktree", "list")["worktrees"])
                opened = wait_for(lambda: next((w for w in owner("workspace.list")["workspaces"]
                                                if w["cwd"] == str(path) and not w.get("host")), None))
                # create already opens it; explicitly close and reopen to
                # exercise worktree.open independently of create's side effect.
                cli("workspace", "close", opened["workspace"])
                cli("worktree", "open", str(path))
                opened = wait_for(lambda: next((w for w in owner("workspace.list")["workspaces"]
                                                if w["cwd"] == str(path) and not w.get("host")), None))
                destination = next(w for w in owner("session.snapshot")["workspaces"]
                                   if w["cwd"] == str(path) and not w.get("host"))
                destination_pane = next(p["pane_id"] for t in destination["tabs"]
                                        if t["active"] for p in t["panes"] if p["kind"] == "terminal")
                source_name = next(w["name"] for w in owner("workspace.list")["workspaces"]
                                   if w["workspace"] == "0")
                # Exercise owner MENU navigation through real client input,
                # not merely successful owner CLI mutations. At this width
                # only the remote projection uses its mobile MENU header.
                fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 80, 0, 0))
                screen = drain(master, 0.7)
                for name, target_pane, suffix in ((opened["name"], destination_pane, "destination"),
                                                  (source_name, temp_pane, "source")):
                    if remote:
                        click(master, *position(screen, "MENU", after_column=20))
                    else:
                        os.write(master, b"\x02M")
                    drain(master)
                    os.write(master, b"\t\t\t" + name.encode() + b"\r")
                    drain(master, 0.5)
                    marker = f"MENU_NAV_{mode}_{suffix}"
                    cli("pane", "run", str(target_pane), "printf '\\n" + marker + "\\n'")
                    observed = bytearray()
                    def visible_destination():
                        observed.extend(drain(master, 0.1))
                        return marker.encode() in observed
                    wait_for(visible_destination)
                    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 81, 0, 0))
                    drain(master, 0.3)
                    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 80, 0, 0))
                    screen = drain(master, 0.7)
                fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 120, 0, 0))
                drain(master, 0.5)
                cli("workspace", "close", opened["workspace"])
                cli("workspace", "focus", "0")
                cli("worktree", "remove", str(path))
                assert not path.exists()
                cli("tab", "focus", temp_tab)

            row("owner worktree create/list/open/remove and real MENU round-trip navigation", worktrees)

            def orchestration():
                task = cli("task", "add", "parity " + mode)["task"]
                assert any(t["id"] == task["id"] for t in owner("task.list")["tasks"])
                cli("task", "get", task["id"])
                cli("task", "delete", task["id"])
                workspace_id = next(w["id"] for w in owner("session.snapshot")["workspaces"] if w["active"])
                # Disabled means no agent can be launched by this test, even
                # if the selected schedule happens to coincide with wall time.
                automation = cli("automation", "create", "parity " + mode, "--disabled", "--every", "86400",
                                 "--title", "Fixture only", "--prompt", "Never executed", "--agent", "codex",
                                 "--workspace-id", workspace_id, "--mode", "workspace")["automation"]
                assert not automation["enabled"]
                assert any(a["id"] == automation["id"] for a in cli("automation", "list")["automations"])
                cli("automation", "get", automation["id"])
                cli("automation", "delete", automation["id"])
                assert not any(a["id"] == automation["id"] for a in owner("automation.list")["automations"])
                cli("agent", "list")

            row("task and disabled automation CRUD; agent CLI owner routing", orchestration)

            def modules():
                module = root / ("module-" + mode)
                module.mkdir()
                evidence = root / ("module-evidence-" + mode)
                module_id = "smoke.parity-" + mode
                command = ["sh", "-c", "printf '%s\\n' \"$HOME\" \"$LUVUS_WORKSPACE_CWD\" > " + shlex.quote(str(evidence))]
                (module / "luvus-module.toml").write_text(
                    f'id = "{module_id}"\nname = "Parity fixture"\nversion = "0.1.0"\nmin_luvus_version = "1.0.99"\n'
                    '[[actions]]\nid = "proof"\ntitle = "Owner proof"\ncontexts = ["workspace", "pane"]\n'
                    + "command = " + json.dumps(command) + "\n")
                cli("module", "link", str(module))
                cli("module", "run", module_id, "proof")
                expected = [str(root / ("remote-home" if remote else "local-home")), str(root / "project")]
                wait_for(lambda: evidence.exists() and evidence.read_text().splitlines() == expected)
                for point, resized in (((80, 15), False), ((118, 27), False), ((118, 27), True)):
                    evidence.unlink()
                    repaint(master)
                    click(master, *point, button=2)
                    menu = drain(master)
                    if resized:
                        fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 34, 132, 0, 0))
                        menu = drain(master, 0.7)
                    x, y = position(menu, "Owner proof")
                    assert 1 <= x <= (132 if resized else 120) and 1 <= y <= (34 if resized else 30)
                    click(master, x, y)
                    try:
                        wait_for(lambda: evidence.exists() and evidence.read_text().splitlines() == expected)
                    except AssertionError as error:
                        raise AssertionError(f"module menu {mode}: anchor={point}, resized={resized}, "
                                             f"click={(x, y)}, evidence={evidence.read_text() if evidence.exists() else None}, "
                                             f"menu={menu!r}") from error
                fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 120, 0, 0))
                drain(master, 0.5)
                cli("module", "unlink", module_id)

            row("module CLI and pane context-menu action execute on owner", modules)

            def close_tab():
                cli("tab", "focus", temp_tab)
                cli("tab", "close", temp_tab)
                assert len(owner("tab.list")["tabs"]) == len(original_tabs)
                cli("tab", "focus", original_active)

            row("temporary tab closes on owner; opposite owner remains unchanged", close_tab)

        def switch(process, master, current, target):
            inventory = json.loads(run("session", "list", "--json").stdout)
            registered = {f'remote-{s["host"]}-{s["session"]}' for s in inventory["remote_sessions"]}
            rows = [(s["name"], False, s["running"], s["name"]) for s in inventory["sessions"]]
            # Public inventory retains its synthetic default contract. The UI
            # omits that placeholder when a real remote default is available.
            default_files = (root / "local-state" / name for name in
                             ("session-origin.json", "session.json", "server.pid", "server.lock"))
            if (not any(path.exists() for path in default_files)
                    and any(s["session"] == "default" for s in inventory["remote_sessions"])):
                rows = [row for row in rows if row[0] != "default"]
            for s in inventory["remote_sessions"]:
                if inventory["merge"] and any(r[0] == s["session"] and not r[1] for r in rows):
                    continue
                name = f'remote-{s["host"]}-{s["session"]}'
                host = next(h for h in inventory["hosts"] if h["host"] == s["host"])
                running = any(r["name"] == s["session"] and r["running"] for r in host["sessions"])
                rows.append((name, True, running, s["session"]))
            rows.sort(key=lambda row: (row[0] != current, row[1], not row[2], row[3].lower()))
            index = 2 + next(i for i, row in enumerate(rows) if row[0] == target)
            os.write(master, b"\x02t")
            screen = drain(master, 1.5)
            display_name = next(row[3] for row in rows if row[0] == target)
            assert display_name.encode() in screen, screen[-5000:]
            if target in registered:
                assert target.encode() not in screen, "routing prefixes must not leak into session labels"
            os.write(master, b"\x1b[H" + b"j" * index + b"\r")
            switched_screen = bytearray()
            started = time.monotonic()
            title = (f"luvus · {display_name} [remote · fake-dev]" if target in registered
                     else f"luvus · {display_name}").encode()
            def changed():
                switched_screen.extend(drain(master, 0.1))
                assert process.poll() is None
                # The persistent client changes its terminal title only after
                # the prepared target handshake. Its PID/argv no longer execs.
                return b"\x1b]0;" + title + b"\x07" in switched_screen
            try:
                wait_for(changed)
            except AssertionError as error:
                raise AssertionError(f"switch {current} -> {target}: index={index}, rows={rows}, "
                                     f"before={screen!r}, after={bytes(switched_screen)!r}") from error
            elapsed = time.monotonic() - started
            switched_screen.extend(drain(master, 1))
            print(f"session switch {current} -> {target}: {elapsed:.3f}s; "
                  f"alternate-screen exits={switched_screen.count(bytes.fromhex('1b5b3f313034396c'))}", flush=True)
            assert b"\x1b[?1049l" not in switched_screen, "switching must not expose the underlying shell"
            assert b"\x1b[?1049h" not in switched_screen, "switching must retain the existing terminal"

        for args in (("--session", "api", "client"), ("session", "attach", "remote-fake-dev-api")):
            process, master = start_client(args)
            remote_view = args[0] == "session"
            drain(master, 2)
            owner_matrix(master, "direct-remote" if remote_view else "local")
            if dimensions_only:
                return
            # Real context-menu click -> owner-side Git choices -> selection.
            os.write(master, b"\x1b[<2;8;4M\x1b[<2;8;4m")
            menu_screen = drain(master)
            assert b"open worktree" in menu_screen.lower(), menu_screen[-5000:]
            click(master, *position(menu_screen, "Open Worktree"))
            choices_screen = drain(master, 1)
            assert b"feature/smoke" in choices_screen and b"feature-checkout" in choices_screen, choices_screen[-5000:]
            os.write(master, b"j\r")
            def opened_worktree():
                workspaces = api("workspace.list", remote=remote_view)["workspaces"]
                return next((w for w in workspaces if w["cwd"] == str(root / "feature-checkout")
                             and not w.get("host")), None)
            worktree_workspace = wait_for(opened_worktree)
            api("workspace.close", {"workspace": worktree_workspace["workspace"]}, remote=remote_view)
            if remote_view:
                assert not projected(), "a standalone view must not enable global merge"
            drain(master)
            if args[0] == "session":
                # Prefix mismatch: a semantic local command must create a tab
                # on the remote owner despite its default Ctrl+Space prefix.
                drain(master, 2)
                before = len(api("tab.list", remote=True)["tabs"])
                os.write(master, b"\x02c")
                wait_for(lambda: len(api("tab.list", remote=True)["tabs"]) == before + 1)
                # Image must be staged on the actual owner, not presentation home.
                os.write(master, b"\x16")
                paths = wait_for(lambda: list((root / "remote-state/sessions/api/clipboard").glob("*.png")))
                assert paths[0].read_bytes() == IMAGE
                assert not list((root / "local-state").glob("**/clipboard/*.png"))
                switch(process, master, "remote-fake-dev-api", "remote-fake-dev-second")
                switch(process, master, "remote-fake-dev-second", "api")
                switch(process, master, "api", "remote-fake-dev-api")
                # Create a session through the actual New Remote form.
                os.write(master, b"\x02t")
                drain(master, 1.5)
                os.write(master, b"\x1b[Hj\r")
                drain(master)
                os.write(master, b"form-created\r")
                form_screen = bytearray()
                def created():
                    form_screen.extend(drain(master, 0.1))
                    return "luvus · form-created [remote · fake-dev]".encode() in form_screen
                wait_for(created)
                assert b"\x1b[?1049l" not in form_screen
                run("--session", "remote-fake-dev-form-created", "ping")
                # A new session created independently while the client is open
                # must appear on the next menu open, without remote add.
                run("--session", "late", "server", "start", remote=True)
                switch(process, master, "remote-fake-dev-form-created", "remote-fake-dev-late")
                switch(process, master, "remote-fake-dev-late", "remote-fake-dev-api")
                # Merge toggle in the standalone remote view must return to
                # the matching local UI with both owners, then remain switchable.
                run("session", "merge", "on")
                merge_screen = bytearray()
                def merged():
                    merge_screen.extend(drain(master, 0.1))
                    return "\x1b]0;luvus · api\x07".encode() in merge_screen
                wait_for(merged)
                assert b"\x1b[?1049l" not in merge_screen
                wait_for(projected)

                # The merged Agents dock must retain owner state and identity,
                # including a non-active tab and pane ID that can collide locally.
                for sequence, state in enumerate(("blocked", "working", "done"), 1):
                    api("agent.report", {"pane": pane, "source": "smoke/remote", "agent": "codex",
                                         "status": state, "sequence": sequence, "ttl_s": 300}, remote=True)
                    def reported():
                        return next((agent for agent in api("agent.list")["agents"]
                                     if agent.get("host") == "fake-dev" and agent["owner_pane"] == pane
                                     and agent["status"] == state), None)
                    agent = wait_for(reported)
                    assert agent["pane"] == f"remote:fake-dev:api:{pane}", agent
                    assert agent["owner_session"] == "api" and agent["cwd"] == str(root / "project")
                screen = repaint(master)
                assert b"fake-dev" in screen and b"codex" in screen, screen[-7000:]
                click(master, *position(screen, "codex", before_column=35))
                wait_for(lambda: any(p["pane"] == pane and p["focused"]
                                     for p in api("pane.list", remote=True)["panes"]))
                wait_for(lambda: any(w.get("host") == "fake-dev" and w["active"]
                                     for w in api("session.snapshot")["workspaces"]))
                owner_matrix(master, "merge")

                # The selectable tree is an outer presentation, not a new
                # owner/session. Exercise it through the real binary transport
                # and mouse parser, then run the existing owner parity matrix.
                identity = lambda: [(w["workspace"], w["workspace_id"], w.get("host"))
                                    for w in api("workspace.list")["workspaces"]]
                before_tree = identity()
                api("config.patch", {"patch": {"layout": {"workspace_display": "tree"}}})
                screen = repaint(master)
                position(screen, "Local machine", before_column=35)
                click(master, *position(screen, "▾ fake-dev", before_column=35))
                screen = repaint(master)
                position(screen, "▸ fake-dev", before_column=35)
                active_remote = next(w["workspace"] for w in api("workspace.list")["workspaces"]
                                     if w["active"] and w.get("host") == "fake-dev")
                api("workspace.focus", {"workspace": active_remote})
                screen = repaint(master)
                click(master, *position(screen, "▾ fake-dev", before_column=35))
                screen = repaint(master)
                click(master, *position(screen, "▸ fake-dev", before_column=35))
                screen = repaint(master)
                _, heading_row = position(screen, "▾ fake-dev", before_column=35)
                click(master, 8, heading_row + 1)
                wait_for(lambda: any(w.get("host") == "fake-dev" and w["active"]
                                     for w in api("session.snapshot")["workspaces"]))
                assert identity() == before_tree
                owner_matrix(master, "merge-tree")
                assert identity() == before_tree
                api("config.patch", {"patch": {"layout": {"workspace_display": "flat"}}})
                screen = repaint(master)
                position(screen, "[fake-dev]", before_column=35)
                assert identity() == before_tree
                print("PASS: tree machine headings fold/unfold, remote row selection, full owner matrix, flat restore and stable workspace identities", flush=True)

                def federated_search():
                    result = api("search.query", {"query": "parity.md", "scope": "files",
                                                  "all_sessions": True})
                    matches = [match for match in result["matches"]
                               if match.get("host") == "fake-dev" and match.get("owner_session") == "api"]
                    assert matches, result
                    assert all(match["kind"] == "file" for match in matches), matches
                    assert all("parity.md" in json.dumps(match["target"]) for match in matches), matches

                case("merge: global search returns host-qualified owner results", federated_search)

                # '+' is machine-explicit in merge mode even from a remote
                # workspace. Both choices open the selected owner's picker.
                screen = repaint(master)
                click(master, *position(screen, "+", before_column=35))
                choices = drain(master)
                assert b"Local machine" in choices and b"fake-dev" in choices, choices[-7000:]
                # x=80 is inside the remote content area beneath the local
                # modal. A leaked click leaves this host chooser open instead
                # of opening the local picker, so the next assertion catches it.
                _, local_row = position(choices, "Local machine", after_column=25)
                click(master, 80, local_row)
                local_picker = drain(master)
                assert str(root / "local-home").encode() in local_picker, local_picker[-7000:]
                remote_before_open = topology(remote=True)
                click(master, *position(local_picker, "picker-local/"))
                local_picker = repaint(master)
                click(master, *position(local_picker, ".."))
                local_picker = repaint(master)
                click(master, *position(local_picker, "picker-local/"))
                drain(master)
                fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 34, 132, 0, 0))
                local_picker = drain(master, 0.7)
                click(master, *position(local_picker, "Open this folder"))
                opened_local = wait_for(lambda: next((w for w in api("workspace.list")["workspaces"]
                                                      if w["cwd"] == str(root / "local-home/picker-local")
                                                      and not w.get("host")), None))
                assert topology(remote=True) == remote_before_open, "local picker changed its remote owner"
                api("workspace.close", {"workspace": opened_local["workspace"]})
                remote_workspace = next(w for w in api("workspace.list")["workspaces"]
                                        if w.get("host") == "fake-dev")
                api("workspace.focus", {"workspace": remote_workspace["workspace"]})
                fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 120, 0, 0))
                drain(master, 0.7)
                os.write(master, b"\x02N")
                choices = drain(master)
                _, chooser_first_row = position(choices, "Local machine", after_column=25)
                _, remote_row = position(choices, "fake-dev", after_column=25,
                                         after_row=chooser_first_row, before_row=chooser_first_row + 3)
                click(master, 80, remote_row)
                remote_picker = drain(master, 1)
                assert str(root / "project").encode() in remote_picker, remote_picker[-7000:]
                click(master, *position(remote_picker, "picker-remote/"))
                remote_picker = repaint(master)
                click(master, *position(remote_picker, ".."))
                drain(master)
                # Navigate to a new folder and open it. Owner API, rather than
                # the rendered path alone, proves which machine mutated.
                os.write(master, b"g" + str(root / "second").encode() + b"\r")
                drain(master)
                fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 34, 132, 0, 0))
                remote_picker = drain(master, 0.7)
                click(master, *position(remote_picker, "Open this folder"))
                workspace = wait_for(lambda: next((w for w in api("workspace.list", remote=True)["workspaces"]
                                                    if w["cwd"] == str(root / "second")), None))
                assert not any(w["cwd"] == str(root / "second") and not w.get("host")
                               for w in api("workspace.list")["workspaces"])
                fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 120, 0, 0))
                drain(master, 0.7)
                print("PASS: merge Open workspace full mouse flow: owner choice, directory/up, resized confirmation; both owners isolated", flush=True)

                # Continue the legal operation chain without reattaching: the
                # new remote workspace's Rename menu must mutate its owner and
                # propagate back, not rename a local projection temporarily.
                wait_for(lambda: len(projected()) == 2)
                local_names = [w["name"] for w in api("workspace.list")["workspaces"] if not w.get("host")]
                screen = repaint(master)
                click(master, *position(screen, "second", before_column=35), button=2)
                menu = drain(master)
                click(master, *position(menu, "Rename"))
                rename_screen = drain(master)
                assert b"second" in rename_screen, rename_screen[-7000:]
                os.write(master, b"\x7f" * len("second") + b"owner-renamed\r")
                wait_for(lambda: any(w["workspace"] == workspace["workspace"] and w["name"] == "owner-renamed"
                                     for w in api("workspace.list", remote=True)["workspaces"]))
                wait_for(lambda: any(w["name"] == "owner-renamed" for w in projected()))
                assert [w["name"] for w in api("workspace.list")["workspaces"] if not w.get("host")] == local_names
                api("workspace.close", {"workspace": workspace["workspace"]}, remote=True)
                wait_for(lambda: len(projected()) == 1)

                # Workspace-native dashboards must be created only by the
                # selected remote owner, never in its local projection.
                for label, kind in (("Open Task Board", "orchestration"),
                                    ("Open Mission Control", "mission_control")):
                    before_local = api("session.snapshot")["workspaces"]
                    local_tabs = sum(len(w["tabs"]) for w in before_local if not w.get("host"))
                    screen = repaint(master)
                    # Local and remote have identical project labels. The
                    # remote row is the second workspace row (2-cell stride).
                    click(master, 8, 6, button=2)
                    menu = drain(master)
                    click(master, *position(menu, label))
                    wait_for(lambda: any(tab["kind"] == kind for w in api("session.snapshot", remote=True)["workspaces"]
                                         for tab in w["tabs"]))
                    after_local = api("session.snapshot")["workspaces"]
                    assert sum(len(w["tabs"]) for w in after_local if not w.get("host")) == local_tabs

                linked = api("workspace.open", {"path": str(root / "feature-checkout")}, remote=True)
                linked_pane = api("pane.list", remote=True)["panes"][0]["pane"]
                api("agent.report", {"pane": linked_pane, "source": "smoke/worktree", "agent": "codex",
                                     "status": "blocked", "ttl_s": 300}, remote=True)
                wait_for(lambda: any(a.get("host") == "fake-dev" and a["owner_pane"] == linked_pane
                                     and a["worktree"] and a["branch"] == "feature/smoke"
                                     for a in api("agent.list")["agents"]))
                api("workspace.close", {"workspace": linked["workspace"]}, remote=True)
                wait_for(lambda: not any(a.get("host") == "fake-dev" and a["owner_pane"] == linked_pane
                                         for a in api("agent.list")["agents"]))

                # The setting applies to other same-name pairs too, including
                # the conventional default session, without another merge call.
                run("--session", "default", "server", "start", remote=True)
                run("--session", "default", "server", "start")
                wait_for(lambda: any(w.get("host") == "fake-dev"
                                     for w in api("session.snapshot", session="default")["workspaces"]))
                os.write(master, b"\x02t")
                menu = drain(master, 1.5)
                assert menu.count(b"default") == 1, menu[-7000:]
                assert b"remote-fake-dev-default" not in menu
                os.write(master, b"\x1b")
                drain(master)
                print("PASS: merged remote Agents status/click/worktree/close, default grouping, "
                      "overlay mouse priority, machine-explicit workspace create/rename, owner TaskBoard/Mission", flush=True)
            os.write(master, b"\x02q")
            process.wait(timeout=5)
            assert process.returncode == 0
            os.close(master)
            clients.pop()

        opened = api("workspace.open", {"path": str(root / "second")}, remote=True)
        index = opened["workspace"]
        wait_for(lambda: len(projected()) == 2)
        run("--host", "fake-dev", "--session", "api", "workspace", "rename", str(index), "renamed-remote")
        wait_for(lambda: any(w["name"] == "renamed-remote" for w in projected()))
        api("workspace.close", {"workspace": index}, remote=True)
        wait_for(lambda: len(projected()) == 1)

        run("session", "merge", "api", "off")
        wait_for(lambda: not projected())
        assert api("ping", remote=True)["session"] == "api"
        run("session", "merge", "api", "on")
        wait_for(projected)
        api("config.patch", {"patch": {"remote_hosts": []}})
        wait_for(lambda: not projected())
        denied = run("--session", "remote-fake-dev-api", "pane", "list", okay=False)
        assert denied.returncode and "Settings > Remote" in denied.stderr
        assert api("ping", remote=True)["session"] == "api"
        api("config.patch", {"patch": {"remote_hosts": ["fake-dev"]}})
        wait_for(projected)
        run("session", "remote", "remove", "remote-fake-dev-api")
        # Removing a cached handle does not deselect its SSH host. Reopening
        # discovery must still find live owners on a selected host.
        discovered = json.loads(run("session", "remote", "list", "--json").stdout)
        assert any(s["host"] == "fake-dev" and s["session"] == "api" for s in discovered["sessions"])
        assert api("ping", remote=True)["session"] == "api"
        assert "unselected" not in (root / "ssh-calls").read_text().splitlines()
        print("PASS: select=connect, discovery, nested names, exact build check, --host CLI, local/remote worktree picker clicks, New Remote form, PTY round trips, prefix mismatch, Ctrl+V owner bytes, merge, live topology, remote process survival")
    finally:
        for process, master in clients:
            process.terminate()
            process.wait(timeout=5)
            os.close(master)
        for remote in (False, True):
            # Only this fixture's two isolated homes; bypass managed-name
            # routing so a local presentation server is not mistaken for SSH.
            state = root / ("remote-state" if remote else "local-state")
            namespaces = state / "sessions"
            # Public inventory intentionally omits hidden presentation servers.
            # Lifecycle cleanup must include every namespace in this fixture only.
            names = {"default"} | ({p.name for p in namespaces.iterdir() if p.is_dir()} if namespaces.is_dir() else set())
            for name in sorted(names):
                assert name and name not in (".", "..") and "/" not in name and "\\" not in name, name
                directory = state if name == "default" else state / "sessions" / name
                assert directory.resolve().is_relative_to(state), name
                run("--session", name, "remote-server-command", "stop", remote=remote, okay=False)
        # Retain failure evidence; successful fixtures are disposable test data.
        if sys.exc_info()[0] is None:
            assert_isolated(env)
            assert_isolated(remote_env)
            shutil.rmtree(root)
        else:
            print("Smoke-test evidence:", root, file=sys.stderr)


if __name__ == "__main__":
    if Path(sys.argv[0]).name == "ssh":
        ssh_substitute()
    elif Path(sys.argv[0]).name == "curl":
        root = fixture_root()
        args = sys.argv[1:]
        assert args[-1] == "https://github.com/kovidgoyal/kitty/releases/download/v0.48.2/kitten-linux-amd64", args
        destination = Path(args[args.index("-o") + 1]).resolve()
        assert destination.parent in (root / "local-state/tools", root / "remote-state/tools"), destination
        with (root / "curl-calls").open("a") as log:
            log.write(str(destination) + "\n")
        if (root / "bad-download").exists():
            destination.write_bytes(b"invalid download")
        else:
            shutil.copyfile(root / "official-kitten", destination)
    elif "--qoder-fixture" in sys.argv:
        fixture_root()
        for line in sys.stdin:
            print("\033[2J\033[999;1H\033]0;Qoder CLI\007" + line.strip(), flush=True)
    elif Path(sys.argv[0]).name in ("xclip", "xsel", "wl-copy", "pbcopy"):
        root = fixture_root()
        if any(option in sys.argv[1:] for option in ("-o", "-out", "--output")):
            sys.stdout.buffer.write(IMAGE)
        else:
            (root / "clipboard-copy").write_bytes(sys.stdin.buffer.read())
    else:
        main()
