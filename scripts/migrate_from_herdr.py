#!/usr/bin/env python3
"""Translate Herdr snapshots, then use Luvus's native restore and SSH admission.

No third-party Python packages. The same worker runs on macOS/Linux over SSH;
only the coordinator reads Herdr's endpoint catalog. Never crawl remote hosts
recursively, replay arbitrary launch commands, or modify a Herdr file.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import uuid

MAX_JSON = 32 * 1024 * 1024
SELECTORS = ("LUVUS_SOCKET_PATH", "LUVUS_SESSION", "LUVUS_REMOTE_HOST",
             "LUVUS_REMOTE_SESSION", "LUVUS_PANE_ID", "LUVUS_API_ADDRESS",
             "LUVUS_SHELL", "LUVUS_ENV", "HERDR_SOCKET_PATH", "HERDR_SESSION")


def read_json(path):
    with Path(path).open("rb") as stream:
        raw = stream.read(MAX_JSON + 1)
    if len(raw) > MAX_JSON:
        raise ValueError("JSON exceeds 32 MiB: " + str(path))
    return json.loads(raw)


def write_new_json(path, value):
    # Exclusive creation also protects existing snapshots and symlink targets.
    with Path(path).open("x", encoding="utf-8") as stream:
        json.dump(value, stream, ensure_ascii=False, indent=2, allow_nan=False)
        stream.write("\n")


def clean_env(home):
    env = dict(os.environ)
    for key in SELECTORS:
        env.pop(key, None)
    env["LUVUS_HOME"] = str(home)
    return env


def run(argv, env=None, input_text=None, timeout=60):
    io_args = {"stdin": subprocess.DEVNULL} if input_text is None else {"input": input_text}
    result = subprocess.run(argv, env=env, universal_newlines=True, encoding="utf-8",
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            timeout=timeout, **io_args)
    if result.returncode:
        # Don't echo argv: remote Python source and agent arguments are private.
        raise RuntimeError("{} exited {}: {}".format(
            Path(argv[0]).name, result.returncode,
            (result.stderr or result.stdout).strip()[-4000:]))
    return result.stdout


def checked_name(name):
    if not isinstance(name, str) or not re.fullmatch(r"[A-Za-z0-9_.-]{1,64}", name) or name in (".", ".."):
        raise ValueError("invalid session name: {!r}; choose a shorter --prefix".format(name))
    return name


def checked_path(value):
    if not isinstance(value, str) or not value.startswith("/") or "\0" in value:
        raise ValueError("expected an absolute Unix directory: {!r}".format(value))
    if not Path(value).is_dir():
        raise ValueError("directory no longer exists: " + value)
    return value


def convert_snapshot(source, shells_only=False):
    """Herdr persist/snapshot.rs -> Luvus persist.rs and layout::LayoutTree.

    Resume references stay data: Luvus's adapters construct and validate resume
    commands. Never translate launch_argv into executable shell text.
    """
    if not isinstance(source, dict) or not isinstance(source.get("workspaces"), list):
        raise ValueError("snapshot must contain a workspaces array")
    if source.get("version", 0) not in (0, 1, 2, 3):
        raise ValueError("unsupported Herdr snapshot version")
    warnings = []
    workspaces = []
    for wi, ws in enumerate(source["workspaces"]):
        tabs = ws.get("tabs", [ws] if "layout" in ws else [])
        if not tabs:
            raise ValueError("workspace {} has no terminal tabs".format(wi))
        root = ws.get("identity_cwd")
        if root is None:  # Herdr's pre-tab snapshot format.
            first = tabs[0]
            root_id = first.get("root_pane") or first.get("focused")
            root = first["panes"].get(str(root_id), next(iter(first["panes"].values())))["cwd"]
        root = checked_path(root)
        new_tabs = []
        for ti, tab in enumerate(tabs):
            if not isinstance(tab.get("panes"), dict) or not tab["panes"]:
                raise ValueError("tab must contain at least one pane")
            panes = []
            seen = set()

            def tree(node, depth=0):
                if depth > 64:
                    raise ValueError("pane layout is too deeply nested")
                if set(node) == {"Pane"}:
                    pid = node["Pane"]
                    if type(pid) is not int or not 0 <= pid <= 0xffffffff or pid in seen:
                        raise ValueError("invalid or repeated pane ID")
                    seen.add(pid)
                    return {"Leaf": pid}
                split = node["Split"]
                direction = split["direction"]
                ratio = split["ratio"]
                if direction not in ("Horizontal", "Vertical") or not 0 < ratio < 1:
                    raise ValueError("invalid split direction/ratio")
                return {"Split": {"axis": 0 if direction == "Horizontal" else 1,
                                  "ratio": ratio, "a": tree(split["first"], depth + 1),
                                  "b": tree(split["second"], depth + 1)}}

            layout = tree(tab["layout"])
            if seen != {int(p) for p in tab["panes"]}:
                raise ValueError("pane layout and pane records do not match")
            for raw_id, pane in tab["panes"].items():
                cwd = checked_path(pane["cwd"])
                ref = pane.get("agent_session")
                native = None
                if ref and not shells_only:
                    value = ref.get("value", "")
                    agent = ref.get("agent", "")
                    # Matches agent::safe_session_id. Unsupported agents are
                    # left to native restore; no guessed resume argv here.
                    if (ref.get("kind") in ("id", "path") and
                            re.fullmatch(r"[A-Za-z0-9_.:/-]{1,256}", value) and
                            re.fullmatch(r"[A-Za-z0-9_-]+", agent)):
                        if ref["kind"] != "path" or Path(value).is_file():
                            native = [agent, value]
                if ref and not native:
                    warnings.append("workspace {} tab {} pane {}: shell only; native reference omitted".format(wi, ti + 1, raw_id))
                elif pane.get("managed_agent_kind") and not ref:
                    warnings.append("workspace {} tab {} pane {}: no native session reference; shell only".format(wi, ti + 1, raw_id))
                if native:
                    warnings.append("workspace {} tab {} pane {}: {} resume delegated to Luvus; verify agent readiness".format(wi, ti + 1, raw_id, native[0]))
                panes.append([int(raw_id), {"cwd": cwd, "command": "",
                    "name": pane.get("agent_name") or pane.get("label"),
                    "backend_label": pane.get("label"), "agent_session": native}])
            focus = tab.get("focused")
            if focus not in seen:
                focus = panes[0][0]
            new_tabs.append({"tree": layout, "focus": focus, "panes": panes,
                             "name": tab.get("custom_name")})
            if tab.get("zoomed"):
                warnings.append("workspace {} tab {}: zoom state is not persisted by Luvus".format(wi, ti + 1))
        active_tab = ws.get("active_tab", 0)
        if type(active_tab) is not int or not 0 <= active_tab < len(new_tabs):
            raise ValueError("invalid active tab")
        workspaces.append({"name": ws.get("custom_name") or Path(root).name or root,
                           "cwd": root, "active_tab": active_tab, "tabs": new_tabs})
    active = source.get("active")
    if active is None:
        active = source.get("selected", 0)
    if type(active) is not int or (workspaces and not 0 <= active < len(workspaces)):
        raise ValueError("invalid active workspace")
    return {"version": 1, "active_ws": active, "workspaces": workspaces}, warnings


def compatible_binary(binary, home):
    resolved = shutil.which(binary)
    if not resolved:
        fallback = Path.home() / ".local/bin" / binary
        if fallback.is_file():
            resolved = str(fallback)
    if not resolved:
        raise ValueError("Luvus not found; install this fork first, or pass --luvus-bin")
    resolved = str(Path(resolved).resolve())
    version = run([resolved, "--version", "--remote-session-protocol"], clean_env(home)).strip()
    if not re.fullmatch(r"luvus \S+ remote-session=2 transport=(?:9|1[0-6]|18|19|20)", version):
        raise ValueError("not a compatible fork binary: " + version)
    return resolved


def api(binary, home, name, method, params=None):
    response = json.loads(run([binary, "--session", name, "uhp", "proxy"],
                              clean_env(home), json.dumps({"id": "herdr-migrate",
                              "method": method, "params": params or {}}) + "\n"))
    if "error" in response:
        raise RuntimeError("{}: {}".format(method, response["error"]))
    return response["result"]


def append_to_default(binary, home, snapshot, warnings):
    """Append through the existing API while an existing owner keeps its PTYs."""
    def call(method, params=None):
        return api(binary, home, "default", method, params)

    original_workspaces = call("workspace.list")["workspaces"]
    original_workspace = next((w for w in original_workspaces if w["active"]), None)
    local_selected = original_workspace and not original_workspace.get("host")
    previous = call("pane.list", {"all_tabs": True})["panes"] if local_selected else []
    focused = next((p["pane"] for p in previous if p["focused"]), None)
    original_tab = next((t for t in call("tab.list")["tabs"] if t["active"]), None) if local_selected else None
    existing = {w["workspace_id"] for w in original_workspaces}
    separate_resume = call("config.get")["config"]["layout"]["resume_in_new_workspace"]
    # Native discovery remains authoritative for a live import. An unavailable
    # reference becomes a shell; do not guess a resume command or a session ID.
    resumable = call("agent.sessions")["sessions"] if any(
        p.get("agent_session") for w in snapshot["workspaces"] for t in w["tabs"] for _, p in t["panes"]) else []
    try:
        for ws in snapshot["workspaces"]:
            for tab in ws["tabs"]:
                anchor = call("terminal.backend.create", {"cwd": ws["cwd"],
                    "placement": {"kind": "workspace"}, "focus": False, "restore": True})
                workspace = anchor["placement"]["workspace"] - 1
                tab_number = anchor["placement"]["tab"]
                locator = {k: anchor[k] for k in ("server_generation", "terminal_id", "pane_id")}
                call("pane.focus", {"pane": anchor["pane_id"]})
                destination = call("workspace.get", {"workspace": workspace})
                if destination["workspace_id"] not in existing:
                    call("workspace.rename", {"workspace": workspace, "name": ws["name"][:40]})
                    existing.add(destination["workspace_id"])
                if tab.get("name"):
                    call("tab.rename", {"tab": tab_number, "name": tab["name"][:40]})
                remap = {}
                for raw_id, pane in tab["panes"]:
                    ref = pane.get("agent_session")
                    # agent.resume currently selects by ID alone. Require that
                    # ID to be unique across agents/cwds before calling it.
                    by_id = [s for s in resumable if ref and s["session_id"] == ref[1]]
                    matches = [s for s in by_id if s["agent"] == ref[0] and s["cwd"] == pane["cwd"]]
                    created = None
                    if len(by_id) == len(matches) == 1 and (not separate_resume or Path(pane["cwd"]).resolve() == Path(ws["cwd"]).resolve()):
                        call("pane.focus", {"pane": anchor["pane_id"]})
                        before = {p["pane"] for p in call("pane.list", {"all_tabs": True})["panes"]}
                        call("agent.resume", {"session_id": ref[1]})
                        after = [p for p in call("pane.list", {"all_tabs": True})["panes"] if p["pane"] not in before]
                        if len(after) != 1 or int(after[0]["workspace"]) != workspace:
                            raise RuntimeError("native resume did not create exactly one pane in the destination workspace")
                        created = after[0]["pane"]
                        call("pane.move", {"pane": created, "tab": tab_number})
                        resumable.remove(matches[0])
                    else:
                        result = call("terminal.backend.create", {"cwd": pane["cwd"],
                            "placement": {"kind": "sibling", "of_terminal": locator},
                            "label": pane.get("backend_label"), "focus": False, "restore": True})
                        created = result["pane_id"]
                        if ref:
                            warnings.append("pane {}: shell only; exact native session unavailable for live import".format(raw_id))
                    remap[raw_id] = int(created)
                    if pane.get("name") and re.fullmatch(r"[a-z][a-z0-9_-]{0,31}", pane["name"]):
                        call("pane.rename", {"pane": created, "name": pane["name"]})
                call("pane.close", {"pane": anchor["pane_id"]})

                def mapped(node):
                    if "Leaf" in node:
                        return {"Leaf": remap[node["Leaf"]]}
                    split = node["Split"]
                    return {"Split": dict(split, a=mapped(split["a"]), b=mapped(split["b"]))}

                call("layout.apply", {"workspace": workspace, "tab": tab_number,
                     "tree": mapped(tab["tree"]), "focus": remap[tab["focus"]]})
    finally:
        if focused:
            call("pane.focus", {"pane": focused})
        elif original_workspace:
            call("workspace.focus", {"workspace_id": original_workspace["workspace_id"]})
            if original_tab:
                call("tab.focus", {"tab_id": original_tab["tab_id"]})


def worker(request):
    """Read/restore on the owner machine, using its own home and directories."""
    home = Path(request.get("home") or Path.home() / ".luvus").expanduser().resolve()
    config = Path(request.get("config") or Path(os.environ.get("XDG_CONFIG_HOME", str(Path.home() / ".config"))) / "herdr").expanduser().resolve()
    paths = [("default", config / "session.json")]
    if (config / "sessions").is_dir():
        paths.extend((p.name, p / "session.json") for p in sorted((config / "sessions").iterdir()) if p.is_dir())
    sessions, errors = [], []
    for source_name, path in paths:
        if not path.is_file():
            continue
        try:
            checked_name(source_name)
            name = checked_name("default" if source_name == "default" else request["prefix"] + source_name)
            snapshot, warnings = convert_snapshot(read_json(path), request["shells_only"])
            if not snapshot["workspaces"]:
                continue
            directory = home if name == "default" else home / "sessions" / name
            marker = directory / "herdr-migration.json"
            identity = {"source": str(path), "shells_only": request["shells_only"]}
            status = "new"
            if marker.exists():
                receipt = read_json(marker)
                if receipt.get("identity") != identity:
                    raise ValueError("destination was imported from another source or with different options: " + str(marker))
                if receipt.get("state") == "incomplete":
                    raise ValueError("previous live import is incomplete; inspect {} before retrying".format(marker))
                status = "already imported"
            elif name == "default" and any((directory / f).exists() for f in ("session.json", "server.pid", "luvus.sock")):
                status = "append to default"
            elif directory.exists() and (name != "default" or any((directory / f).exists() for f in ("session.json", "server.pid", "luvus.sock"))):
                raise ValueError("destination exists: {}; use another --prefix".format(directory))
            sessions.append({"name": name, "source": str(path), "directory": str(directory),
                             "snapshot": snapshot, "warnings": warnings, "status": status,
                             "identity": identity})
        except (ValueError, KeyError, TypeError, OSError) as error:
            errors.append("{}: {}".format(path, error))
    if request["action"] == "apply":
        binary = compatible_binary(request.get("binary") or "luvus", home)
        for session in sessions:
            directory = Path(session["directory"])
            try:
                if session["status"] == "new":
                    directory.mkdir(parents=True, exist_ok=directory == home, mode=0o700)
                    write_new_json(directory / "session.json", session["snapshot"])
                    write_new_json(directory / "herdr-migration.json", {"identity": session["identity"]})
                # A rerun may finish a previous import whose server failed to
                # start. 'server start' reuses an owner; never restart one.
                run([binary, "--session", session["name"], "server", "start"], clean_env(home))
                if session["status"] == "append to default":
                    marker = directory / "herdr-migration.json"
                    write_new_json(marker, {"identity": session["identity"], "state": "incomplete"})
                    append_to_default(binary, home, session["snapshot"], session["warnings"])
                    marker.write_text(json.dumps({"identity": session["identity"], "state": "complete"}) + "\n")
                api(binary, home, session["name"], "terminal.backend.inventory")
                session["status"] = "opened"
            except (ValueError, OSError, RuntimeError, subprocess.TimeoutExpired) as error:
                errors.append("{}: {}".format(session["name"], error))
                session["status"] = "failed"
    # Keep snapshot data on its owner. The coordinator needs only counts/status.
    for session in sessions:
        snapshot = session.pop("snapshot")
        session["workspaces"] = len(snapshot["workspaces"])
        session["panes"] = sum(len(t["panes"]) for w in snapshot["workspaces"] for t in w["tabs"])
        session.pop("identity")
    return {"home": str(home), "config": str(config), "sessions": sessions, "errors": errors}


def remote_worker(target, request):
    if not target or target.startswith("-") or any(c in target for c in "\r\n\0"):
        raise ValueError("invalid SSH target")
    source = Path(__file__).read_text(encoding="utf-8")
    command = "python3 -c {} --worker".format(shlex.quote(source))
    return json.loads(run(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10",
                           "-o", "StrictHostKeyChecking=yes", "--", target, command],
                          input_text=json.dumps(request), timeout=300))


def ssh_alias(target, dry_run):
    """Use OpenSSH to resolve Herdr targets, including user@alias and ssh://.

    Luvus admits literal Host aliases. Save OpenSSH's effective configuration
    under one private alias rather than inventing a second SSH option parser.
    Credentials remain paths/agent references, never copied key material.
    """
    digest = hashlib.sha256(target.encode()).hexdigest()[:12]
    label = re.sub(r"[^A-Za-z0-9_-]", "-", target)[:24].strip("-") or "host"
    alias = "herdr-{}-{}".format(label, digest)
    if dry_run:
        return alias
    ssh_dir = Path.home() / ".ssh"
    ssh_dir.mkdir(mode=0o700, exist_ok=True)
    directory = ssh_dir / "luvus-herdr"
    directory.mkdir(mode=0o700, exist_ok=True)
    config = ssh_dir / "config"
    include = 'Include "{}"\n'.format(directory / "*.conf")
    original = config.read_text() if config.exists() else ""
    path = directory / (digest + ".conf")
    if not path.exists():
        effective = run(["ssh", "-G", "--", target])
        body = "Host " + alias + "\n"
        body += "".join("  " + line + "\n" for line in effective.splitlines() if not line.startswith("host "))
        with path.open("x") as stream:
            stream.write(body)
    if not original.startswith(include):
        if config.is_symlink():
            raise ValueError("SSH config is a symlink; use --host-map TARGET=EXISTING_ALIAS")
        if config.exists():
            backup = ssh_dir / ("config.before-herdr-" + uuid.uuid4().hex[:12])
            shutil.copy2(config, backup)
            print("SSH config backup: " + str(backup), flush=True)
        temporary = ssh_dir / (".config-herdr-" + uuid.uuid4().hex)
        with temporary.open("x") as stream:
            # Reset the Include's final Host context before the original file.
            stream.write(include + "Host *\n" + original)
        os.replace(str(temporary), str(config))
    # Validate through OpenSSH before admitting the alias to Luvus.
    run(["ssh", "-G", "--", alias])
    return alias


def show(owner, result):
    print("{}: {}".format(owner, result["config"]), flush=True)
    if not result["sessions"] and not result["errors"]:
        print("  No saved nonempty sessions found.", flush=True)
    for session in result["sessions"]:
        print("  {}: {}, {} workspaces, {} panes".format(session["name"], session["status"], session["workspaces"], session["panes"]), flush=True)
        for warning in session["warnings"]:
            print("    NOTE: " + warning, flush=True)
    for error in result["errors"]:
        print("  ERROR: " + error, file=sys.stderr, flush=True)


def main():
    parser = argparse.ArgumentParser(description="Migrate Herdr local and saved SSH sessions into this Luvus fork. Requires Python 3.6+ on every machine.")
    parser.add_argument("--dry-run", action="store_true", help="read source snapshots and report destinations; write nothing")
    parser.add_argument("--local-only", action="store_true", help="do not read or connect saved remote machines")
    parser.add_argument("--remote", action="append", default=[], metavar="TARGET", help="also discover this SSH target (repeatable)")
    parser.add_argument("--host-map", action="append", default=[], metavar="TARGET=ALIAS", help="reuse an existing literal SSH alias instead of generating one")
    parser.add_argument("--prefix", default="herdr-", help="named session prefix (default: herdr-); default always maps to default")
    parser.add_argument("--shells-only", action="store_true", help="omit native agent resume references")
    parser.add_argument("--install-remote", action="store_true", help="allow host add --install --yes for missing/incompatible remote Luvus")
    parser.add_argument("--herdr-config", help="local Herdr config directory (default: $XDG_CONFIG_HOME/herdr or ~/.config/herdr)")
    parser.add_argument("--herdr-state", help="local Herdr state directory (default: $XDG_STATE_HOME/herdr or ~/.local/state/herdr)")
    parser.add_argument("--remote-herdr-config", help="Herdr config directory on remote machines")
    parser.add_argument("--luvus-home", default=os.environ.get("LUVUS_HOME"), help="local destination (default: $LUVUS_HOME or ~/.luvus)")
    binary_options = parser.add_mutually_exclusive_group()
    binary_options.add_argument("--luvus-bin", default="luvus", help="local fork binary")
    binary_options.add_argument("--install", action="store_true", help="run the fork installer locally and allow remote installation before migration")
    args = parser.parse_args()
    if sys.platform not in ("linux", "darwin"):
        parser.error("only macOS and Linux are supported")
    if args.local_only and args.remote:
        parser.error("--local-only cannot be combined with --remote")
    os.umask(0o077)
    home = Path(args.luvus_home or Path.home() / ".luvus").expanduser().resolve()
    request = {"action": "plan", "prefix": args.prefix, "shells_only": args.shells_only,
               "config": args.herdr_config, "home": str(home), "binary": args.luvus_bin}
    local = worker(request)
    show("local", local)
    targets = set(args.remote)
    if not args.local_only:
        state = Path(args.herdr_state or Path(os.environ.get("XDG_STATE_HOME", str(Path.home() / ".local/state"))) / "herdr").expanduser()
        catalog = state / "client/endpoints.json"
        if catalog.exists():
            saved = read_json(catalog)
            if saved.get("version") != 1:
                raise ValueError("unsupported Herdr endpoint catalog version")
            targets.update(row["target"] for row in saved["ssh"] if row["enabled"])
    mappings = {}
    for pair in args.host_map:
        target, separator, alias = pair.partition("=")
        if not separator or not re.fullmatch(r"[A-Za-z0-9_-][A-Za-z0-9_.-]*", alias):
            parser.error("--host-map requires TARGET=ALIAS")
        mappings[target] = alias
    remote_request = dict(request, config=args.remote_herdr_config, home=None, binary="luvus")
    remotes, failed = {}, bool(local["errors"])
    for target in sorted(targets):
        try:
            result = remote_worker(target, remote_request)
            show(target, result)
            remotes[target] = result
            failed |= bool(result["errors"])
        except (ValueError, OSError, RuntimeError, subprocess.TimeoutExpired) as error:
            print("ERROR {}: {}".format(target, error), file=sys.stderr, flush=True)
            failed = True
    if args.dry_run:
        print("Preview only. No files, hosts or servers changed.")
        return int(failed)
    if not local["sessions"] and not any(r["sessions"] for r in remotes.values()):
        print("No Herdr session snapshots found; check --herdr-config and --herdr-state.", file=sys.stderr)
        return 1
    # Refuse a known incomplete plan before opening any of its sessions.
    if failed:
        print("Resolve the preflight errors before migrating.", file=sys.stderr)
        return 1
    selected_binary = args.luvus_bin
    if args.install:
        installer = Path(__file__).resolve().parent.parent / "install.sh"
        install_dir = Path(os.environ.get("LUVUS_INSTALL_DIR") or Path.home() / ".local/bin").expanduser().resolve()
        print("Installing this fork with install.sh...", flush=True)
        print(run(["sh", str(installer)],
                  dict(clean_env(home), LUVUS_INSTALL_DIR=str(install_dir)), timeout=180), end="", flush=True)
        # Use the exact installed binary, even if PATH still selects another one.
        selected_binary = str(install_dir / "luvus")
    binary = compatible_binary(selected_binary, home)
    local = worker(dict(request, action="apply", binary=binary))
    show("local", local)
    failed |= bool(local["errors"])
    for target in remotes:
        try:
            alias = mappings.get(target) or ssh_alias(target, False)
            flags = ["--install", "--yes"] if args.install or args.install_remote else []
            run([binary, "host", "add", alias, *flags, "--json"], clean_env(home), timeout=240)
            result = remote_worker(target, dict(remote_request, action="apply"))
            show(target, result)
            failed |= bool(result["errors"])
            for session in result["sessions"]:
                if session["status"] != "opened":
                    continue
                run([binary, "session", "remote", "add", alias, session["name"], "--merge", "--json"], clean_env(home))
                print("Remote ready: {} / {}".format(alias, session["name"]), flush=True)
        except (ValueError, OSError, RuntimeError, subprocess.TimeoutExpired) as error:
            print("ERROR {}: {}".format(target, error), file=sys.stderr, flush=True)
            failed = True
    for session in local["sessions"]:
        if session["status"] == "opened":
            print("Open: " + " ".join(shlex.quote(v) for v in [binary, "--session", session["name"]]))
    print("Herdr was left running. New shells use Luvus's configured shell. PTY processes, scrollback, shell variables and arbitrary launch argv are not transferred.")
    return int(failed)


if __name__ == "__main__":
    try:
        if sys.version_info < (3, 6):
            raise RuntimeError("Python 3.6 or newer is required on every machine")
        if sys.argv[1:] == ["--worker"]:
            os.umask(0o077)
            print(json.dumps(worker(json.load(sys.stdin))))
        else:
            sys.exit(main())
    except (ValueError, KeyError, TypeError, OSError, RuntimeError, subprocess.TimeoutExpired) as exc:
        print("error: " + str(exc), file=sys.stderr)
        sys.exit(1)
