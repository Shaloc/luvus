#!/usr/bin/env python3
"""Isolated real Codex + Luvus hook routing and selective native hook trust.

Requires Python 3.11+, Codex with app-server/hooks, and cargo build.
All servers/files are below target/. No model service or trust bypass is used.
"""
from contextlib import contextmanager
import copy
import json
import os
from pathlib import Path
import selectors
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
import tomllib


@contextmanager
def app_server(codex, env, root, overrides=()):
    args = [codex, *overrides, "--enable", "hooks", "-c", 'model_provider="fixture"',
            "-c", 'model="fixture"', "-c",
            'model_providers.fixture={name="fixture",base_url="http://127.0.0.1:9/v1",wire_api="responses",requires_openai_auth=false,request_max_retries=0,stream_max_retries=0}',
            "app-server", "--stdio"]
    with (root / "codex-stderr.log").open("a") as errors:
        process = subprocess.Popen(args, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                   stderr=errors, env=env, cwd=root)
        selector = selectors.DefaultSelector()
        selector.register(process.stdout, selectors.EVENT_READ)
        buffer = b""
        events = []
        identifier = 0

        def read(deadline):
            nonlocal buffer
            while b"\n" not in buffer:
                assert time.monotonic() < deadline, "Codex response timed out"
                if not selector.select(0.1):
                    continue
                data = os.read(process.stdout.fileno(), 65536)
                assert data, "Codex app-server exited"
                buffer += data
            line, buffer = buffer.split(b"\n", 1)
            return json.loads(line)

        def request(method, params):
            nonlocal identifier
            identifier += 1
            process.stdin.write((json.dumps({"id": identifier, "method": method, "params": params}) + "\n").encode())
            process.stdin.flush()
            deadline = time.monotonic() + 15
            while True:
                value = read(deadline)
                if value.get("id") == identifier:
                    assert "result" in value, value
                    return value["result"]
                events.append(value)

        def turn(config):
            thread = request("thread/start", {"cwd": str(root), "sessionStartSource": "startup", "config": config})["thread"]["id"]
            result = request("turn/start", {"threadId": thread, "input": [{"type": "text", "text": "fixture"}]})
            deadline = time.monotonic() + 15
            # A model request follows the synchronous prompt hooks. The fixture
            # endpoint refuses it; interrupt there instead of waiting for retries.
            while not any(event.get("method") in ("turn/completed", "error") and event.get("params", {}).get("threadId") == thread for event in events):
                events.append(read(deadline))
            if not any(event.get("method") == "turn/completed" and event.get("params", {}).get("threadId") == thread for event in events):
                request("turn/interrupt", {"threadId": thread, "turnId": result["turn"]["id"]})
            return thread

        try:
            request("initialize", {"clientInfo": {"name": "luvus-hook-test", "version": "1"}, "capabilities": {"experimentalApi": True}})
            process.stdin.write(b'{"method":"initialized"}\n')
            process.stdin.flush()
            yield request, turn
        finally:
            selector.close()
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
            process.stdin.close()
            process.stdout.close()


def main():
    repo = Path(__file__).resolve().parent.parent
    luvus = repo / "target/debug/luvus"
    codex = shutil.which("codex")
    assert codex and luvus.is_file()
    root = Path(tempfile.mkdtemp(prefix="codex-hook-", dir=repo / "target"))
    env = {k: v for k, v in os.environ.items() if not k.startswith(("LUVUS_", "CODEX_"))}
    env.update(LUVUS_HOME=str(root / "luvus"), CODEX_HOME=str(root / "codex home '🐈"),
               SHELL="/bin/sh", PS1="", PS2="")
    started = passed = False

    def run(*args):
        result = subprocess.run([str(luvus), "--session", "hook-test", *args],
                                env=env, cwd=root, text=True, capture_output=True, timeout=15)
        assert result.returncode == 0, (args, result.stdout, result.stderr)
        return result.stdout

    def api(method, params=None):
        result = subprocess.run([str(luvus), "--session", "hook-test", "uhp", "proxy"],
                                input=json.dumps({"id": "test", "method": method, "params": params or {}}) + "\n",
                                env=env, cwd=root, text=True, capture_output=True, timeout=10)
        value = json.loads(result.stdout)
        assert "result" in value, value
        return value["result"]

    def agents():
        return {a["pane"]: a["session"] for a in api("agent.list")["agents"]}

    try:
        home = Path(env["CODEX_HOME"])
        home.mkdir()
        sentinel = root / "unrelated-hook.py"
        sentinel.write_text("import json,pathlib,sys\np=json.load(sys.stdin)\npathlib.Path(" + repr(str(root / "unrelated-")) + " + p['session_id']).touch()\n")
        unrelated = {"type": "command", "command": shlex.join([sys.executable, str(sentinel)])}
        (home / "hooks.json").write_text(json.dumps({"hooks": {"UserPromptSubmit": [{"hooks": [unrelated]}]}}))
        (home / "hooks.json").chmod(0o600)
        run("integration", "install", "codex")
        assert (home / "hooks.json").stat().st_mode & 0o777 == 0o600
        run("server", "start")
        started = True
        first = api("pane.list")["panes"][0]["pane"]
        second = api("pane.split", {"pane": first, "direction": "right"})["pane"]
        info = api("session.status", {"name": "hook-test"})["session"]
        assert Path(info["session_dir"]).resolve().is_relative_to(root)
        launcher = root / "luvus/integrations/bin/codex"
        fake_dir = root / "fake tools"
        fake_dir.mkdir()
        fake = fake_dir / "codex"
        fake.write_text("#!/usr/bin/env python3\nimport json,sys\nprint(json.dumps(sys.argv[1:]))\n")
        fake.chmod(0o755)
        routes = []
        for pane in (first, second):
            caller = dict(env, LUVUS_ENV="1", LUVUS_PANE_ID=pane,
                          LUVUS_SOCKET_PATH=info["socket_path"], LUVUS_BIN_PATH=str(luvus),
                          PATH=os.pathsep.join([str(launcher.parent), str(fake_dir), env["PATH"]]))
            args = json.loads(subprocess.check_output([str(launcher), "resume", "fixture-session"], env=caller, text=True, timeout=5))
            assert args[-2:] == ["resume", "fixture-session"]
            assert "--dangerously-bypass-hook-trust" not in args
            config = {"hooks": {}}
            for index in range(0, len(args) - 2, 2):
                assert args[index] == "-c"
                config["hooks"].update(tomllib.loads(args[index + 1])["hooks"])
            for passthrough in (["--version"], ["app-server", "daemon", "start"], ["exec", "fixture"], ["resume", "--remote=unix://fixture"]):
                actual = json.loads(subprocess.check_output([str(launcher), *passthrough], env=caller, text=True, timeout=5))
                assert actual == passthrough, actual
            explicit = json.loads(subprocess.check_output([str(launcher), "--dangerously-bypass-hook-trust"], env=caller, text=True, timeout=5))
            assert explicit[-1] == "--dangerously-bypass-hook-trust"
            # Query the startup flags exactly as the real TUI's hook browser does.
            with app_server(codex, env, root, args[:-2]) as (request, _):
                inventory = request("hooks/list", {"cwds": [str(root)]})["data"][0]["hooks"]
            owned = [hook for hook in inventory if hook["source"] == "sessionFlags" and "LUVUS_CODEX_HOOK_CONTEXT=1" in hook.get("command", "")]
            assert len(owned) == 2 and all(hook["trustStatus"] == "untrusted" for hook in owned), inventory
            states = {hook["key"]: {"trusted_hash": hook["currentHash"]} for hook in owned}
            routes.append((config, states))
            other = next(hook for hook in inventory if hook.get("command") == unrelated["command"])
        print("PASS: native hook browser discovers both pane hooks; launcher adds no trust bypass", flush=True)

        # Nested homes may prepend more than one private launcher.
        nested = root / "nested-launcher"
        nested.mkdir()
        shutil.copy2(launcher, nested / "codex")
        caller["PATH"] = os.pathsep.join([str(launcher.parent), str(nested), str(fake_dir), env["PATH"]])
        assert json.loads(subprocess.check_output([str(launcher), "--version"], env=caller, text=True, timeout=5)) == ["--version"]
        print("PASS: administrative/remote commands and explicit trust flags pass through; nested launchers do not recurse", flush=True)

        daemon_env = {k: v for k, v in env.items() if not k.startswith("LUVUS_")}
        daemon_env.update(LUVUS_ENV="1", LUVUS_PANE_ID="999999", LUVUS_SOCKET_PATH="missing.sock")
        subprocess.run([str(home / "luvus-agent-hook.sh")], input='{"session_id":"stale"}',
                       env=daemon_env, text=True, check=True, timeout=5)
        with app_server(codex, daemon_env, root) as (_, turn):
            thread = turn(routes[0][0])
            assert first not in agents(), "an untrusted hook ran"
            assert not (root / ("unrelated-" + thread)).exists()
            print("PASS: unreviewed hooks remain blocked", flush=True)
            sessions = []
            for pane, (config, states) in zip((first, second), routes):
                # Emulate approval of these exact two definitions only. Production
                # uses Codex's native Review hooks / t UI, not this fixture code.
                trusted = copy.deepcopy(config)
                trusted["hooks"]["state"] = states
                thread = turn(trusted)
                sessions.append(thread)
                assert agents().get(pane) == thread, (pane, agents())
                assert not (root / ("unrelated-" + thread)).exists(), "trust leaked to an unrelated hook"
            assert agents()[first] == sessions[0] and agents()[second] == sessions[1]
            print("PASS: selective native trust binds two same-cwd panes through one stale-environment daemon", flush=True)
            changed = copy.deepcopy(trusted)
            for event in ("SessionStart", "UserPromptSubmit"):
                changed["hooks"][event][0]["hooks"][0]["command"] += " "
            thread = turn(changed)
            assert agents()[second] == sessions[1], "changed hook reused old trust"
            print("PASS: a changed command requires fresh trust", flush=True)
            trusted["hooks"]["state"] = dict(trusted["hooks"]["state"], **{other["key"]: {"trusted_hash": other["currentHash"]}})
            thread = turn(trusted)
            assert agents()[second] == thread and (root / ("unrelated-" + thread)).exists()
            print("PASS: existing user hooks are preserved and execute when separately trusted; quoted paths work", flush=True)
        passed = True
    finally:
        if started:
            run("server", "stop")
        if passed:
            shutil.rmtree(root)
        else:
            print("Evidence:", root, file=sys.stderr)


if __name__ == "__main__":
    main()
