#!/usr/bin/env python3
"""Opt-in real QoderCLI prompt smoke test (uses the configured model/account).

Run after cargo build. All Luvus state and the Qoder working directory live in
target/. Never connects to an inherited server. No tools, MCP, or codebase
backflow are needed. Requires an already authenticated QoderCLI installation.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import uuid


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--live", action="store_true", help="allow real model requests")
    args = parser.parse_args()
    if not args.live:
        parser.error("--live is required: this test makes real model requests")
    repo = Path(__file__).resolve().parent.parent
    binary = repo / "target/debug/luvus"
    assert binary.is_file() and shutil.which("qodercli"), "build Luvus and install QoderCLI first"
    root = Path(tempfile.mkdtemp(prefix="qoder-prompt-", dir=repo / "target"))
    env = {k: v for k, v in os.environ.items() if not k.startswith("LUVUS_")}
    env.update(LUVUS_HOME=str(root / "luvus"), SHELL="/bin/sh", PS1="", PS2="",
               QODER_FEATURE_CODEBASE_BACKFLOW="0")
    command = [str(binary), "--session", "qoder-prompt-test"]
    started = False

    def run(*argv, timeout=20):
        process = subprocess.run(command + list(argv), env=env, cwd=root,
                                 text=True, capture_output=True, timeout=timeout)
        assert process.returncode == 0, (argv, process.stdout, process.stderr)
        return process.stdout

    def api(method, params):
        process = subprocess.run(command + ["uhp", "proxy"], env=env, cwd=root,
                                 input=json.dumps({"id": "smoke", "method": method, "params": params}) + "\n",
                                 text=True, capture_output=True, timeout=20)
        response = json.loads(process.stdout)
        assert "result" in response, response
        return response["result"]

    def read():
        return json.loads(run("agent", "read", "qoder-test", "--source", "visible", "--lines", "80"))["result"]["text"]

    def wait_for(predicate, label, timeout=90):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            text = read()
            if predicate(text):
                return text
            time.sleep(0.2)
        (root / "failure.txt").write_text(text)
        raise AssertionError(f"{label}: no acceptance evidence; screen saved to {root / 'failure.txt'}")

    try:
        print(f"Binary: {binary}\nHome: {env['LUVUS_HOME']}\nEvidence: {root}", flush=True)
        run("server", "start")
        started = True
        info = api("session.status", {"name": "qoder-prompt-test"})["session"]
        assert Path(info["session_dir"]).resolve().is_relative_to(root)
        print(f"Session: qoder-prompt-test\nSocket: {info['socket_path']}", flush=True)
        pane = api("pane.list", {})["panes"][0]["pane"]
        result = json.loads(run("agent", "start", "qoder-test", "--kind", "qodercli",
                                "--pane", pane, "--timeout", "45", "--",
                                "--cwd", str(root), "--dangerously-skip-permissions",
                                "--strict-mcp-config", "--mcp-config", '{"mcpServers":{}}',
                                timeout=55))
        assert result["result"]["ready"], result
        wait_for(lambda text: "Type your message" in text, "initial composer", timeout=20)
        # Exercise both CLI aliases and the legacy JSON API. A nonce assembled
        # by the model cannot be satisfied by echoed prompt text in the editor.
        for index, surface in enumerate(("prompt", "send", "legacy", "single")):
            suffix = uuid.uuid4().hex[:10]
            expected = "QODER_OK_" + suffix
            prompt = ("This is a terminal input smoke test.\n"
                      "Do not use tools or inspect files.\n"
                      "Do not change files or settings.\n"
                      "Ignore repository work; only answer this message.\n"
                      f"Reply with the concatenation of QODER_OK_ and {suffix}.\n"
                      "  Preserve this indented context: 中文 $PATH 'quotes'.\n"
                      "Reply once, then wait.")
            if surface == "single":
                prompt = f"Do not use tools. Reply only with the concatenation of QODER_OK_ and {suffix}."
            if surface == "legacy":
                response = api("agent.send", {"target": "qoder-test", "text": prompt})
                assert response["type"] == "agent_send", response
            else:
                response = json.loads(run("agent", "prompt" if surface == "single" else surface, "qoder-test", prompt))["result"]
                assert response["submitted"] and response["evidence"] == "queued", response
            text = wait_for(lambda text: any(line.strip().lstrip("▪•● ") == expected
                                            for line in text.splitlines())
                            and "Type your message" in text,
                            f"{surface} model response")
            assert "[Pasted Text:" not in text, text
            (root / f"{index}-{surface}.txt").write_text(text)
            print(f"PASS: {surface} received prompt and replied {expected}; no extra Enter", flush=True)
    finally:
        if started:
            run("server", "stop")


if __name__ == "__main__":
    main()
