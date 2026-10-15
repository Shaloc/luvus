#!/usr/bin/env python3
"""Offline fixtures plus real isolated Luvus owners. No production/network use.

python3 scripts/test-migrate-from-herdr.py target/debug/luvus
The SSH substitute shares the existing remote-session smoke's owner routing.
The driver needs Python 3.9+; MIGRATE_TEST_PYTHON selects the migration workers.
"""

import copy
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time

REPO = Path(__file__).resolve().parent.parent


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


MIGRATE = load("migrate", REPO / "scripts/migrate_from_herdr.py")


def ssh_helper():
    root = Path(os.environ["LUVUS_SMOKE_ROOT"]).resolve()
    assert root.parent == REPO / "target" and root.name.startswith("remote-smoke-")
    assert (root / ".isolated-smoke").read_text() == str(root)
    args = sys.argv[2:]
    if "-G" in args:
        # Exercise real OpenSSH config resolution without any connection.
        os.execv(os.environ["MIGRATE_REAL_SSH"], ["ssh", *args])
    stripped = args[:]
    while stripped and stripped[0].startswith("-"):
        if stripped[0] == "--":
            stripped.pop(0)
            break
        stripped = stripped[2:] if stripped[0] in ("-o", "-S") else stripped[1:]
    host, *command = stripped
    if host == "offline":
        return 255
    assert host == "fake-dev" or host.startswith("herdr-fake-dev-"), host
    if len(command) == 1 and command[0].startswith("python3 -c "):
        payload = shlex.split(command[0])
        assert payload == ["python3", "-c", (REPO / "scripts/migrate_from_herdr.py").read_text(), "--worker"]
        env = remote_env(root)
        python = os.environ["MIGRATE_WORKER_PYTHON"]
        os.execve(python, [python, *payload[1:]], env)
    # Reuse the existing fake SSH transport with its private owner home.
    # It maps LUVUS_HOME to remote-state; the worker's default ~/.luvus is a
    # fixture symlink to that same directory.
    sys.argv = ["ssh", "fake-dev", *command]
    load("remote_smoke", REPO / "scripts/test-remote-sessions.py").ssh_substitute()


def remote_env(root):
    env = dict(os.environ)
    for key in MIGRATE.SELECTORS:
        env.pop(key, None)
    env.update(HOME=str(root / "remote-home"), LUVUS_HOME=str(root / "remote-state"))
    for key in ("CONFIG", "STATE", "CACHE", "DATA"):
        env["XDG_" + key + "_HOME"] = str(root / "remote-home" / key.lower())
    return env


def curl_helper():
    """Serve only fixture migration files and release assets; never network."""
    root = Path(os.environ["LUVUS_SMOKE_ROOT"]).resolve()
    assert root.parent == REPO / "target" and root.name.startswith("remote-smoke-")
    assert (root / ".isolated-smoke").read_text() == str(root)
    args = sys.argv[2:]
    output = None
    while len(args) > 1:
        option = args.pop(0)
        if option == "-fsSL":
            continue
        assert option in ("-o", "--connect-timeout", "--max-time"), option
        value = args.pop(0)
        if option == "-o":
            output = Path(value).resolve()
            assert root in output.parents
    url, = args
    with (root / "downloads/calls").open("a") as log:
        log.write(url + "\n")
    base = "https://raw.githubusercontent.com/Shaloc/luvus/main/"
    if url.startswith(base):
        relative = url[len(base):]
        assert relative in ("install.sh", "scripts/migrate_from_herdr.py")
        assert output is not None
        if os.environ.get("MIGRATE_MISSING_HELPER") and relative.endswith(".py"):
            print("fixture download failed", file=sys.stderr)
            return 22
        shutil.copyfile(str(REPO / relative), str(output))
        return 0
    if url == "https://api.github.com/repos/Shaloc/luvus/releases/latest":
        print('{"tag_name":"fork-migration-fixture"}')
        return 0
    assert url.startswith("https://github.com/Shaloc/luvus/releases/download/fork-migration-fixture/")
    asset = root / "downloads" / url.rsplit("/", 1)[1]
    assert asset.is_file() and output is not None
    if os.environ.get("MIGRATE_BAD_CHECKSUM") and asset.suffix == ".sha256":
        output.write_text("0" * 64 + "\n")
    else:
        shutil.copyfile(str(asset), str(output))
    return 0


def fixture(path, second):
    return {"version": 3, "active": 0, "selected": 0, "workspaces": [{
        "custom_name": "Herdr project", "identity_cwd": str(path), "active_tab": 1,
        "tabs": [{"custom_name": "build", "focused": 2, "root_pane": 1,
                  "layout": {"Split": {"direction": "Horizontal", "ratio": 0.35,
                      "first": {"Pane": 1}, "second": {"Pane": 2}}},
                  "panes": {"1": {"cwd": str(path), "label": "shell-main"},
                            "2": {"cwd": str(second), "label": "输出窗口", "launch_argv": ["touch", str(path / "BAD")]}},
                  "zoomed": False},
                 {"custom_name": "notes", "focused": 3, "layout": {"Pane": 3},
                  "panes": {"3": {"cwd": str(path), "agent_name": "notes"}}}]}]}


def main():
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else "target/debug/luvus").resolve()
    assert binary.is_file() and (REPO / "target") in binary.parents, binary
    worker_python = shutil.which(os.environ.get("MIGRATE_TEST_PYTHON", sys.executable))
    assert worker_python, "MIGRATE_TEST_PYTHON must select an installed interpreter"
    print("Migration workers: " + MIGRATE.run([worker_python, "--version"]).strip(), flush=True)
    (REPO / "target").mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(dir=REPO / "target", prefix="remote-smoke-migration-") as tmp:
        root = Path(tmp)
        (root / ".isolated-smoke").write_text(str(root))
        for directory in ("local-home", "remote-home", "local-state", "remote-state", "bin", "downloads", "project", "project/sub 'quoted';$"):
            (root / directory).mkdir(parents=True, exist_ok=True)
        (root / "remote-home/.luvus").symlink_to(root / "remote-state", target_is_directory=True)
        (root / "bin/luvus").symlink_to(binary)
        (root / "bin/python3").symlink_to(worker_python)
        (root / "remote-home/.local/bin").mkdir(parents=True)
        (root / "remote-home/.local/bin/luvus").symlink_to(binary)
        (root / "bin/ssh").write_text("#!/bin/sh\nexec {} {} --ssh-helper \"$@\"\n".format(shlex.quote(sys.executable), shlex.quote(str(Path(__file__).resolve()))))
        (root / "bin/ssh").chmod(0o755)
        (root / "bin/curl").write_text("#!/bin/sh\nexec {} {} --curl-helper \"$@\"\n".format(shlex.quote(sys.executable), shlex.quote(str(Path(__file__).resolve()))))
        (root / "bin/curl").chmod(0o755)
        standalone = root / "standalone migration.sh"
        standalone.write_text((REPO / "migrate_from_herdr.sh").read_text())
        payload = ('#!/bin/sh\nprintf "%s\\n" "$*" >> "$MIGRATE_INSTALL_CALLS"\nexec ' + shlex.quote(str(binary)) + ' "$@"\n').encode()
        for target in ("x86_64-unknown-linux-musl", "aarch64-apple-darwin"):
            archive = root / "downloads" / ("luvus-" + target + ".tar.gz")
            with tarfile.open(str(archive), "w:gz") as tar:
                info = tarfile.TarInfo("luvus-fixture/luvus")
                info.mode, info.size = 0o755, len(payload)
                tar.addfile(info, io.BytesIO(payload))
            archive.with_suffix(".gz.sha256").write_text(hashlib.sha256(archive.read_bytes()).hexdigest() + "\n")
        env = dict(os.environ, HOME=str(root / "local-home"), LUVUS_HOME=str(root / "local-state"),
                   LUVUS_SMOKE_ROOT=str(root), LUVUS_SMOKE_BINARY=str(binary),
                   LUVUS_INSTALL_DIR=str(root / "install with spaces"),
                   MIGRATE_INSTALL_CALLS=str(root / "installed-calls"), MIGRATE_WORKER_PYTHON=worker_python,
                   MIGRATE_REAL_SSH=shutil.which("ssh"), PATH=str(root / "bin") + os.pathsep + os.environ["PATH"])
        for key in MIGRATE.SELECTORS:
            env.pop(key, None)
        env.pop("PI_CODING_AGENT_SESSION_DIR", None)
        env.pop("LUVUS_VERSION", None)
        for key in tuple(env):
            if key.startswith("LUVUS_SMOKE_") and key not in ("LUVUS_SMOKE_ROOT", "LUVUS_SMOKE_BINARY"):
                env.pop(key)
        env.pop("LUVUS_TEST_SSH_POOL_CONFIG", None)
        for key in ("CONFIG", "STATE", "CACHE", "DATA"):
            env["XDG_" + key + "_HOME"] = str(root / "local-home" / key.lower())
        old_env = dict(os.environ)
        os.environ.clear()
        os.environ.update(env)
        remenv = remote_env(root)
        for owner in ("local", "remote"):
            (root / (owner + "-state/config.json")).write_text(json.dumps({"check_updates": False, "shell": "/bin/sh", "language": "en"}))
            source = root / (owner + "-home/config/herdr")
            source.mkdir(parents=True)
            (source / "session.json").write_text(json.dumps(fixture(root / "project", root / "project/sub 'quoted';$")))
            (source / "sessions/work").mkdir(parents=True)
            (source / "sessions/work/session.json").write_text(json.dumps(fixture(root / "project", root / "project")))
        state = root / "local-home/state/herdr/client"
        state.mkdir(parents=True)
        (state / "endpoints.json").write_text(json.dumps({"version": 1, "ssh": [
            {"target": "fake-dev", "session": "default", "enabled": True},
            {"target": "fake-dev", "session": "work", "enabled": True},
            {"target": "offline", "session": "default", "enabled": False}]}))
        ssh = root / "local-home/.ssh"
        ssh.mkdir()
        (ssh / "config").write_text("Host fake-dev\n  HostName 192.0.2.1\n  User fixture\n  Port 2222\n  IdentityFile ~/.ssh/fixture_key\n")
        sources = {p: p.read_bytes() for p in root.glob("*-home/config/herdr/**/session.json")}

        def cli(*args, remote=False, text=None):
            return MIGRATE.run([str(binary), *args], remenv if remote else env, text)

        def api(method, params=None, remote=False, session="default"):
            value = json.loads(cli("--session", session, "uhp", "proxy", remote=remote,
                text=json.dumps({"id": "test", "method": method, "params": params or {}}) + "\n"))
            assert "result" in value, value
            return value["result"]

        def owner_panes(remote=False, session="default"):
            workspaces = api("workspace.list", remote=remote, session=session)["workspaces"]
            owned = {w["workspace"] for w in workspaces if not w.get("host")}
            return [p for p in api("pane.list", {"all_tabs": True}, remote=remote, session=session)["panes"] if p["workspace"] in owned]

        def selected_workspace(workspace_id):
            # Remote focus completes after the owner supplies its first frame.
            # The focus API response acknowledges the request before that frame.
            deadline = time.monotonic() + 10
            while True:
                selected = next(w for w in api("workspace.list")["workspaces"] if w["active"])
                if selected["workspace_id"] == workspace_id:
                    return selected
                assert time.monotonic() < deadline, {"selected": selected, "expected": workspace_id}
                time.sleep(.05)

        def migrate(*args, ok=True, mode="checkout"):
            # Deliberately poison inherited selectors. The script must ignore them.
            poisoned = dict(env, LUVUS_SOCKET_PATH=str(root / "never-contact.sock"),
                            LUVUS_SESSION="unrelated", LUVUS_REMOTE_HOST="offline")
            binary_args = [] if "--install" in args else ["--luvus-bin", str(binary)]
            entries = {"checkout": [str(REPO / "migrate_from_herdr.sh")],
                       "pipe": ["sh", "-s", "--"], "standalone": ["sh", str(standalone)]}
            result = subprocess.run([*entries[mode], *binary_args, *args],
                                    input=standalone.read_text() if mode == "pipe" else None,
                                    env=poisoned, cwd=root / "project", capture_output=True, text=True, timeout=120)
            assert (result.returncode == 0) == ok, result.stdout + result.stderr
            assert not list((root / "local-home/cache/luvus").glob(".herdr-migration.*"))
            return result.stdout + result.stderr

        try:
            converted, warnings = MIGRATE.convert_snapshot(fixture(root / "project", root / "project"))
            assert converted["workspaces"][0]["tabs"][0]["tree"]["Split"]["axis"] == 0
            assert converted["workspaces"][0]["active_tab"] == 1
            invalid = copy.deepcopy(fixture(root / "project", root / "project"))
            invalid["workspaces"][0]["tabs"][0]["layout"]["Split"]["ratio"] = float("nan")
            try:
                MIGRATE.convert_snapshot(invalid)
                raise AssertionError("invalid ratio accepted")
            except ValueError:
                pass
            print("PASS: snapshot conversion and malformed layout rejection", flush=True)

            before = {str(p): p.read_bytes() for p in root.rglob("*") if p.is_file() and not p.is_symlink()}
            preview = migrate("--dry-run", "--install")
            after = {str(p): p.read_bytes() for p in root.rglob("*") if p.is_file() and not p.is_symlink()}
            assert before == after, set(after) - set(before)
            assert "default: new" in preview and "herdr-work: new" in preview
            assert "herdr-default" not in preview
            print("PASS: local/remote dry run writes nothing and maps default directly", flush=True)

            for mode in ("pipe", "standalone"):
                preview = migrate("--dry-run", "--install", mode=mode)
                assert "default: new" in preview and "Preview only" in preview
            assert not (root / "install with spaces/luvus").exists()
            assert not list((root / "local-state").rglob("session.json"))
            assert not list((root / "remote-state").rglob("session.json"))
            env["MIGRATE_MISSING_HELPER"] = "1"
            assert "fixture download failed" in migrate("--install", mode="pipe", ok=False)
            env.pop("MIGRATE_MISSING_HELPER")
            assert not (root / "install with spaces/luvus").exists()
            assert not (root / "installed-calls").exists()
            print("PASS: pipe and standalone bootstrap preserve preview; failed downloads abort and clean up", flush=True)

            # Existing default with a live, named shell and layout. Its process,
            # terminal identity, contents and focus must survive the live import.
            cli("--session", "default", "server", "start")
            sentinel = api("terminal.backend.create", {"cwd": str(root / "project"),
                "placement": {"kind": "workspace"}, "focus": True, "restore": True, "label": "existing"})
            sentinel_id = sentinel["pane_id"]
            before_panes = api("pane.list", {"all_tabs": True})["panes"]
            env["MIGRATE_BAD_CHECKSUM"] = "1"
            assert "SHA-256 mismatch" in migrate("--shells-only", "--install", mode="pipe", ok=False)
            env.pop("MIGRATE_BAD_CHECKSUM")
            assert not (root / "install with spaces/luvus").exists()
            assert not (root / "local-state/herdr-migration.json").exists()
            assert len(owner_panes()) == len(before_panes)
            print("PASS: installer checksum failure leaves sessions untouched", flush=True)
            output = migrate("--shells-only", "--install", mode="pipe")
            assert "Remote ready:" in output, output
            assert (root / "install with spaces/luvus").read_bytes() == payload
            assert "host add " in (root / "installed-calls").read_text()
            assert "--install --yes --json" in (root / "installed-calls").read_text()
            print("PASS: one command installs with the existing installer and migrates using the installed binary", flush=True)
            inventory = api("terminal.backend.inventory")
            retained = next(p for p in inventory["terminals"] if p["pane_id"] == sentinel_id)
            assert retained["terminal_id"] == sentinel["terminal_id"] and retained["root_process"] == sentinel["root_process"]
            panes = owner_panes()
            assert len(panes) == len(before_panes) + 3, panes
            assert next(p for p in panes if p["focused"])["pane"] == sentinel_id
            assert any(t["label"] == "输出窗口" for t in inventory["terminals"])
            new_ids = {p["pane"] for p in panes} - {p["pane"] for p in before_panes}
            build_pane = next(p for p in panes if p["pane"] in new_ids and p["cwd"].endswith("'quoted';$"))
            layout = api("layout.export", {"workspace": int(build_pane["workspace"]), "tab": int(build_pane["tab"])})
            assert abs(layout["tree"]["Split"]["ratio"] - .35) < .0001
            api("pane.run", {"pane": build_pane["pane"], "command": "printf 'MIGRATED_SHELL_OK\\n'"})
            deadline = time.monotonic() + 8
            while "MIGRATED_SHELL_OK" not in api("pane.read", {"pane": build_pane["pane"]})["text"]:
                assert time.monotonic() < deadline
                time.sleep(.1)
            assert len(owner_panes(remote=True)) == 3
            named_panes = owner_panes(session="herdr-work")
            assert len(named_panes) == 3, named_panes
            print("PASS: existing default keeps its PTY/focus; imported layouts, labels, cwd and shells work", flush=True)

            count = len(panes)
            migrate("--shells-only")
            assert len(owner_panes()) == count
            assert len(owner_panes(remote=True)) == 3
            assert not (root / "project/BAD").exists()
            assert all(p.read_bytes() == content for p, content in sources.items())
            assert not (root / "local-state/sessions/herdr-default").exists()
            print("PASS: rerun does not duplicate panes; Herdr snapshots and arbitrary argv remain untouched", flush=True)

            migrate("--shells-only", "--prefix", "", "--dry-run")
            assert not (root / "local-state/sessions/work").exists()
            (root / "local-state/sessions/conflict-work").mkdir()
            collision = migrate("--shells-only", "--prefix", "conflict-", "--local-only", ok=False)
            assert "destination exists" in collision
            migrate("--local-only", "--dry-run", ok=False)  # shell-only receipt differs
            missing = copy.deepcopy(fixture(root / "project", root / "project"))
            missing["workspaces"][0]["tabs"][0]["panes"]["2"]["cwd"] = str(root / "missing")
            try:
                MIGRATE.convert_snapshot(missing)
                raise AssertionError("missing cwd accepted")
            except ValueError:
                pass
            print("PASS: occupied destinations, changed import options and missing cwd fail closed", flush=True)

            native_source = root / "native-source/sessions/native"
            native_source.mkdir(parents=True)
            native = fixture(root / "project", root / "project")
            native["workspaces"][0]["tabs"][0]["panes"]["1"]["agent_session"] = {
                "source": "herdr:pi", "agent": "pi", "kind": "id", "value": "fixture-native"}
            (native_source / "session.json").write_text(json.dumps(native))
            (root / "bin/pi").write_text("#!/bin/sh\nprintf 'NATIVE_RESUME:%s\\n' \"$*\"\nexec /bin/sh -i\n")
            (root / "bin/pi").chmod(0o755)
            migrate("--local-only", "--herdr-config", str(root / "native-source"))
            native_panes = owner_panes(session="herdr-native")
            assert len(native_panes) == 3
            deadline = time.monotonic() + 10
            while not any("NATIVE_RESUME:--session fixture-native" in api("pane.read", {"pane": p["pane"]}, session="herdr-native")["text"] for p in native_panes):
                assert time.monotonic() < deadline, "native adapter did not resume the exact reference"
                time.sleep(.1)
            print("PASS: real native restore invokes the existing Pi adapter with the exact session reference", flush=True)

            store = root / "local-home/.pi/agent/sessions"
            store.mkdir(parents=True)
            (store / "live.jsonl").write_text(json.dumps({"id": "fixture-live", "cwd": str(root / "project")}) + "\n")
            native["workspaces"][0]["tabs"][0]["panes"]["1"]["agent_session"]["value"] = "fixture-live"
            converted, warnings = MIGRATE.convert_snapshot(native)
            original_ids = {p["pane"] for p in owner_panes()}
            workspaces = api("workspace.list")["workspaces"]
            local = next(w for w in workspaces if not w.get("host") and w["cwd"] == str(root / "project"))
            api("workspace.rename", {"workspace_id": local["workspace_id"], "name": "Keep local name"})
            # The same directory on another owner must never receive a local
            # imported pane, even when that projection is first and selected.
            remote = next(w for w in workspaces if w.get("host") and w["cwd"] == str(root / "project"))
            api("workspace.move", {"workspace_id": remote["workspace_id"], "to": 0})
            api("workspace.focus", {"workspace_id": remote["workspace_id"]})
            selected_workspace(remote["workspace_id"])
            remote_before = api("terminal.backend.inventory", remote=True)["terminals"]
            converted["workspaces"][0]["cwd"] = str(root / "project" / ".." / "project")
            MIGRATE.append_to_default(str(binary), root / "local-state", converted, warnings)
            selected = selected_workspace(remote["workspace_id"])
            assert selected["tabs"] == remote["tabs"]
            assert api("workspace.get", {"workspace_id": local["workspace_id"]})["name"] == "Keep local name"
            remote_after = api("terminal.backend.inventory", remote=True)["terminals"]
            assert [(t["terminal_id"], t["root_process"]) for t in remote_after] == [(t["terminal_id"], t["root_process"]) for t in remote_before]
            migrate("--shells-only")  # completed receipts also work with a remote selection
            selected_workspace(remote["workspace_id"])
            api("workspace.focus", {"workspace_id": local["workspace_id"]})
            new_panes = [p for p in owner_panes() if p["pane"] not in original_ids]
            assert len(new_panes) == 3, new_panes
            deadline = time.monotonic() + 10
            while not any("NATIVE_RESUME:--session fixture-live" in api("pane.read", {"pane": p["pane"]})["text"] for p in new_panes):
                assert time.monotonic() < deadline, warnings
                time.sleep(.1)
            retained = next(t for t in api("terminal.backend.inventory")["terminals"] if t["pane_id"] == sentinel_id)
            assert retained["terminal_id"] == sentinel["terminal_id"] and retained["root_process"] == sentinel["root_process"]
            print("PASS: live default preserves local/remote ownership, remote focus, existing names and native resume", flush=True)

            failure = migrate("--dry-run", "--remote", "offline", ok=False)
            assert "ERROR offline" in failure
            print("PASS: unreachable remote returns failure without claiming completion", flush=True)
        finally:
            # Stop only the exact fixture owners, never discover production.
            for remote in (False, True):
                for name in ("default", "herdr-work", "herdr-native"):
                    subprocess.run([str(binary), "--session", name, "server", "stop"],
                                   env=remenv if remote else env, stdout=subprocess.DEVNULL,
                                   stderr=subprocess.DEVNULL, timeout=15)
            os.environ.clear()
            os.environ.update(old_env)


if __name__ == "__main__":
    if sys.argv[1:2] == ["--ssh-helper"]:
        sys.exit(ssh_helper())
    if sys.argv[1:2] == ["--curl-helper"]:
        sys.exit(curl_helper())
    main()
