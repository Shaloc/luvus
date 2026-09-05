#!/usr/bin/env python3
"""Unix smoke test: real Luvus servers/PTY clients with a local SSH substitute.

Run after cargo build: python3 scripts/test-remote-sessions.py
Use --restart-only for the isolated server restart --all smoke test.
All homes, sockets, files and child processes are isolated below target/.
No production server or actual SSH destination is accessed.
"""

import base64
import json
import os
from pathlib import Path
import re
import select
import shutil
import subprocess
import sys
import tempfile
import time
import unicodedata

IMAGE = base64.b64decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aN1sAAAAASUVORK5CYII=")


def ssh_substitute():
    args = sys.argv[1:]
    while args and args[0].startswith("-"):
        args = args[2:] if args[0] == "-o" else args[1:]
    host, *command = args
    root = Path(os.environ["LUVUS_SMOKE_ROOT"])
    with (root / "ssh-calls").open("a") as log:
        log.write(host + "\n")
    if host == "fake-old":
        print("luvus 1.0.99")
        raise SystemExit(0)
    if host != "fake-dev":
        raise SystemExit("unexpected SSH destination: " + host)
    os.environ["HOME"] = str(root / "remote-home")
    os.environ["LUVUS_HOME"] = str(root / "remote-state")
    for key in ("LUVUS_SOCKET_PATH", "LUVUS_SESSION", "LUVUS_REMOTE_HOST", "LUVUS_REMOTE_SESSION"):
        os.environ.pop(key, None)
    os.execv(os.environ["LUVUS_SMOKE_BINARY"], command)


def main():
    repo = Path(__file__).resolve().parent.parent
    restart_only = "--restart-only" in sys.argv[1:]
    positional = [arg for arg in sys.argv[1:] if arg != "--restart-only"]
    binary = (Path(positional[0]) if positional else repo / "target/debug/luvus").resolve()
    if not binary.is_file():
        raise SystemExit("Build Luvus first: cargo build --locked")
    root = Path(tempfile.mkdtemp(prefix="remote-smoke-", dir=repo / "target"))
    clients = []
    env = dict(os.environ)
    for key in ("LUVUS_SOCKET_PATH", "LUVUS_SESSION", "LUVUS_REMOTE_HOST", "LUVUS_REMOTE_SESSION",
                "LUVUS_PANE_ID", "LUVUS_API_ADDRESS", "LUVUS_SHELL"):
        env.pop(key, None)
    for directory in ("local-home/.ssh", "local-state", "remote-home", "remote-state", "bin", "project", "second"):
        (root / directory).mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "init", "--quiet", str(root / "project")], check=True)
    subprocess.run(["git", "-C", str(root / "project"), "-c", "user.name=Smoke",
                    "-c", "user.email=smoke@example.invalid", "commit", "--quiet",
                    "--allow-empty", "-m", "fixture"], check=True)
    subprocess.run(["git", "-C", str(root / "project"), "worktree", "add", "--quiet",
                    "-b", "feature/smoke", str(root / "feature-checkout")], check=True)
    # A symlink invokes this same file's narrow SSH-substitute role.
    (root / "bin/ssh").symlink_to(Path(__file__).resolve())
    (root / "local-home/.ssh/config").write_text("Host fake-dev fake-old unselected\n  HostName 192.0.2.1\n")
    (root / "bin/xclip").symlink_to(Path(__file__).resolve())
    for state in ("local-state", "remote-state"):
        (root / state / "config.json").write_text(json.dumps({
            "check_updates": False, "remote_hosts": [], "shell": "/bin/sh",
            "prefix": "ctrl+b" if state == "local-state" else "ctrl+space",
        }))
    env.update(HOME=str(root / "local-home"), LUVUS_HOME=str(root / "local-state"),
               PATH=str(root / "bin") + ":" + env.get("PATH", ""), TERM="xterm-256color",
               LUVUS_SMOKE_ROOT=str(root), LUVUS_SMOKE_BINARY=str(binary), DISPLAY=":smoke")
    env.pop("WAYLAND_DISPLAY", None)
    remote_env = dict(env, HOME=str(root / "remote-home"), LUVUS_HOME=str(root / "remote-state"))

    def run(*args, remote=False, input_text=None, okay=True):
        result = subprocess.run([str(binary), *args], env=remote_env if remote else env,
                                cwd=root / "project", input=input_text, text=True,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=25)
        if okay and result.returncode:
            raise AssertionError(f"{args}: {result.stdout}\n{result.stderr}")
        return result

    def api(method, params=None, remote=False, session="api"):
        response = run("--session", session, "uhp", "proxy", remote=remote,
                       input_text=json.dumps({"id": "smoke", "method": method, "params": params or {}}) + "\n")
        return json.loads(response.stdout)["result"]

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
        denied = run("--host", "unselected", "pane", "list", okay=False)
        assert denied.returncode and "Settings > Remote" in denied.stderr
        assert not (root / "ssh-calls").exists(), "disabled hosts must be rejected before SSH"
        run("--session", "api", "server", "start", remote=True)
        run("--session", "second", "server", "start", remote=True)
        run("--session", "api", "server", "start")
        api("config.patch", {"patch": {"remote_hosts": ["fake-dev", "fake-old"]}})
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

        def position(screen, label, before_column=None, after_column=0):
            text = screen.decode("utf-8", errors="replace")
            text = re.sub(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)", "", text)
            text = re.sub(r"\x1b\[[0-9;?]*[A-GI-Za-z]", "", text)
            for move in re.finditer(r"\x1b\[(\d+);(\d+)H([^\x1b]*)", text):
                chunk = move.group(3)
                if label not in chunk:
                    continue
                prefix = chunk[:chunk.index(label)]
                width = sum(0 if unicodedata.combining(char) else
                            2 if unicodedata.east_asian_width(char) in ("W", "F") else 1
                            for char in prefix)
                x, y = int(move.group(2)) + width, int(move.group(1))
                if x >= after_column and (before_column is None or x < before_column):
                    return x, y
            raise AssertionError(f"clickable label {label!r} not rendered: {text[-7000:]}")

        def click(master, x, y, button=0):
            os.write(master, f"\x1b[<{button};{x};{y}M\x1b[<{button};{x};{y}m".encode())

        def switch(process, master, current, target):
            inventory = json.loads(run("session", "list", "--json").stdout)
            registered = {f'remote-{s["host"]}-{s["session"]}' for s in inventory["remote_sessions"]}
            rows = [(s["name"], False, s["running"], s["name"]) for s in inventory["sessions"]
                    if s["name"] not in registered]
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
            wait_for(changed)
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
            # Real context-menu click -> owner-side Git choices -> selection.
            os.write(master, b"\x1b[<2;8;4M\x1b[<2;8;4m")
            menu_screen = drain(master)
            assert b"open worktree" in menu_screen.lower(), menu_screen[-5000:]
            os.write(master, b"\x1b[<0;10;9M\x1b[<0;10;9m")
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
                os.write(master, b"\x1b")
                drain(master)
                os.write(master, b"\x02N")
                choices = drain(master)
                _, remote_row = position(choices, "fake-dev", after_column=25)
                click(master, 80, remote_row)
                remote_picker = drain(master, 1)
                assert str(root / "project").encode() in remote_picker, remote_picker[-7000:]
                # Navigate to a new folder and open it. Owner API, rather than
                # the rendered path alone, proves which machine mutated.
                os.write(master, b"g" + str(root / "second").encode() + b"\r")
                drain(master)
                os.write(master, b"\r")
                workspace = wait_for(lambda: next((w for w in api("workspace.list", remote=True)["workspaces"]
                                                    if w["cwd"] == str(root / "second")), None))
                assert not any(w["cwd"] == str(root / "second") and not w.get("host")
                               for w in api("workspace.list")["workspaces"])

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
            inventory = json.loads(run("remote-session-list", remote=remote).stdout)
            for session in inventory:
                if session["running"]:
                    run("--session", session["name"], "remote-server-command", "stop",
                        remote=remote, okay=False)
        # Retain failure evidence; successful fixtures are disposable test data.
        if sys.exc_info()[0] is None:
            shutil.rmtree(root)
        else:
            print("Smoke-test evidence:", root, file=sys.stderr)


if __name__ == "__main__":
    if Path(sys.argv[0]).name == "ssh":
        ssh_substitute()
    elif Path(sys.argv[0]).name == "xclip":
        sys.stdout.buffer.write(IMAGE)
    else:
        main()
