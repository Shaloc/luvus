#!/usr/bin/env python3
# Luvus Codex launcher: carry this invocation's pane route across the daemon.
import json
import os
from pathlib import Path
import shlex
import shutil
import sys


def interactive(args):
    # Administrative commands must never seed a shared daemon with a pane route.
    values = {"-c", "--config", "-m", "--model", "-p", "--profile", "-s",
              "--sandbox", "-a", "--ask-for-approval", "-C", "--cd", "--add-dir",
              "-i", "--image", "--enable", "--disable", "--local-provider",
              "--remote-auth-token-env"}
    commands = {"agents", "exec", "e", "review", "login", "logout", "mcp",
                "plugin", "app-server", "remote-control", "completion", "update",
                "doctor", "sandbox", "debug", "apply", "queue", "archive",
                "delete", "migrate-rollouts", "unarchive", "cloud", "exec-server",
                "features", "help"}
    skip = False
    command = None
    for arg in args:
        if skip:
            skip = False
            continue
        if arg in ("-h", "--help", "-V", "--version", "--remote") or arg.startswith("--remote="):
            return False
        if arg in values:
            skip = True
        elif arg == "--":
            break
        elif not arg.startswith("-") and command is None:
            command = arg
    return command not in commands


def main():
    directory = Path(__file__).resolve().parent
    paths = [entry for entry in os.environ.get("PATH", "").split(os.pathsep)
             if Path(entry or ".").resolve() != directory]
    executable = shutil.which("codex", path=os.pathsep.join(paths))
    if not executable:
        sys.exit("Luvus: codex is not installed on the remaining PATH")
    # Nested Luvus homes can put two launchers on PATH. Each must remove itself
    # before delegating, otherwise the two wrappers would resolve each other.
    os.environ["PATH"] = os.pathsep.join(paths)
    args = sys.argv[1:]
    route = {key: os.environ.get(key, "") for key in
             ("LUVUS_ENV", "LUVUS_SOCKET_PATH", "LUVUS_PANE_ID", "LUVUS_BIN_PATH")}
    if interactive(args) and route["LUVUS_ENV"] == "1" and all(route.values()):
        home = Path(os.environ.get("CODEX_HOME") or Path.home() / ".codex")
        script = home / "luvus-agent-hook.sh"
        if script.is_file() and os.access(script, os.X_OK):
            # Session-scoped config travels with thread/start or thread/resume.
            # No daemon environment, cwd guess, or globally saved pane ID is used.
            command = shlex.join(["env", "-u", "LUVUS_SESSION",
                                  "LUVUS_CODEX_HOOK_CONTEXT=1",
                                  *[key + "=" + value for key, value in route.items()],
                                  str(script)])
            overrides = []
            for event in ("SessionStart", "UserPromptSubmit"):
                # TOML inline tables, with JSON string escaping for literal paths.
                matcher = 'matcher="startup|resume",' if event == "SessionStart" else ''
                value = '[{' + matcher + 'hooks=[{type="command",command=' + json.dumps(command, ensure_ascii=False) + ',timeout=5}]}]'
                overrides.extend(["-c", "hooks." + event + "=" + value])
            args = overrides + args
    os.execv(executable, [executable, *args])


if __name__ == "__main__":
    main()
