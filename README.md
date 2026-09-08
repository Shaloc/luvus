# Luvus

<div align="center">

<img src="assets/logo.png" alt="Luvus logo" width="220" />

**Mission control for your AI coding agents.**

[![release](https://img.shields.io/github/v/release/Shaloc/luvus)](https://github.com/Shaloc/luvus/releases/latest)
[![CLI builds](https://github.com/Shaloc/luvus/actions/workflows/fork-macos-build.yml/badge.svg)](https://github.com/Shaloc/luvus/actions/workflows/fork-macos-build.yml)
[![docs](https://img.shields.io/badge/docs-luvus.dev-c6ff1a.svg)](https://luvus.dev/docs/)
![license](https://img.shields.io/badge/license-Apache--2.0-blue.svg)
![platforms](https://img.shields.io/badge/platforms-macOS%20·%20Linux%20·%20Windows-lightgrey.svg)

**[Upstream website](https://luvus.dev)** · **[Documentation](https://luvus.dev/docs/)** · **[Fork releases](https://github.com/Shaloc/luvus/releases)**

<br />

<a href="assets/video.mp4"><img src="assets/video.gif" alt="Luvus with split panes, a live agent sidebar, and a built-in Git dashboard" width="820" /></a>

</div>

This is the [Shaloc/luvus](https://github.com/Shaloc/luvus) fork of
[RizRiyz/luvus](https://github.com/RizRiyz/luvus), with Qoder CLI, One Dark and
One Light themes, and managed SSH sessions with same-name session merging.
Remote-only sessions stay remote: merge reuses an existing local owner but
does not create a local copy. Stopped sessions can be deleted with confirmation
from the session row's context menu, with a separate target for each machine.
Choose a flat workspace list or a collapsible machine tree in
**Menu > Settings > Layout > Workspace display**.
Tabs stay in their chosen workspace by default, even when an agent launches
commands in another project. **Layout > Auto-move tabs by directory** opts into
automatic workspace creation and tab movement based on scanned process directories
(`layout.auto_workspace_rehome: true`). Terminal directory and branch tracking
remain active with this setting off.

## Features

- **Persistent workspaces:** Open, rename, pin, and switch projects. A background
  server keeps tabs, panes, layouts, terminal state, and named sessions alive.
- **Complete pane and tab control:** Split, resize, zoom, move, name, focus, run,
  inspect, close, reorder, and swap with the mouse, TUI, or CLI.
- **Agent awareness:** Detect supported agents automatically and show blocked,
  working, done, or idle state with session titles, tokens, cost, context use,
  and optional sound alerts.
- **Agent workflows:** Start, name, message, inspect, wait for, resume, and send
  keys to agents. Fork Claude, Grok, Codex, Pi, and OMP sessions with their
  context intact.
- **Files and code:** Browse a Git-aware file tree, inspect files and changes,
  reveal paths, and open files in a pane, tab, preview, or external editor.
- **Git and GitHub:** View status, branches, commits, contributors, pull
  requests, issues, and repository activity without leaving Luvus.
- **Worktrees and orchestration:** Create worktrees, coordinate dependent tasks,
  reserve file paths, assign agents, schedule timezone-aware recurring work,
  run quality gates, and merge completed work.
- **Remote and multi-client use:** Register SSH-config hosts, target remote
  panes and worktrees with `--host`, merge same-name local and remote
  workspaces with hostname tags, or use the raw SSH attach escape hatch.
- **Terminal tools:** Configure per-pane Scrollback Memory, search across pane
  history, use copy mode, click detected links, and run full-screen terminal apps.
- **Extensible surfaces:** Install modules with actions, events, settings,
  startup hooks, panes, sidebar docks, and Top or Bottom Luvus Bar widgets.
- **Universal Harness Protocol:** Build harnesses and orchestrators on the
  single versioned UHP 1.0 method registry, with owner-only local IPC,
  snapshots, event streams, exact input, and semantic waits.
- **Custom interface:** Move and resize two sidebars, remap keys and the prefix,
  use presets, select from 8 languages, and install composable local or
  community themes.
- **Fork releases:** Download macOS Apple Silicon and Linux x86_64 binaries, and
  inspect the environment with `luvus doctor`.

## Install

Install or update this fork on **macOS Apple Silicon** or **Linux x86_64**:

```sh
curl -fsSL https://raw.githubusercontent.com/Shaloc/luvus/main/install.sh | sh
```

The installer downloads from [this fork's releases](https://github.com/Shaloc/luvus/releases/latest),
verifies SHA-256, and installs to `~/.local/bin`. GitHub CLI and sudo are not
required. An existing binary is backed up before replacement. Running sessions
are left untouched.

Set `LUVUS_INSTALL_DIR` to replace a binary installed elsewhere. For an update,
check `command -v luvus` first so an older copy does not take precedence on `PATH`.

```sh
export PATH="$HOME/.local/bin:$PATH"
luvus --version --remote-session-protocol
# When ready, restart all sessions on this machine to load the new binary:
luvus server restart --all
```

Install matching releases on the local and SSH host machines for managed remote
sessions. To upgrade, rerun the installer above; `luvus update`, the upstream
website installers, Homebrew tap, and crates.io package still target upstream.
Prebuilt fork releases currently cover only the two platforms listed above.

After configuring an SSH alias, `luvus host add devbox --install` enables it
and prompts before installing a missing/incompatible fork binary on that host,
without restarting servers. Settings → Remote also offers **Offer Luvus installation
when selecting a host**, off by default. Both paths require confirmation for
each host; the dialog defaults to Cancel. Unattended CLI use requires explicit
`--install --yes`. The policy applies only to subsequent explicit host selections;
discovery and reconnect never install. See [remote setup](https://github.com/Shaloc/luvus/blob/main/website/src/content/docs/docs/guides/remote.mdx).

To build this fork from source with a Rust toolchain:

```sh
git clone https://github.com/Shaloc/luvus.git
cd luvus
cargo build --release --locked --bin luvus
```

## Quick start

```bash
luvus          # launch or reattach to your session
luvus doctor   # check your setup: git, gh, ssh
```

Run Luvus in a project, split a pane, and start an agent. Luvus detects supported
agents automatically.

**macOS:** Disable *Select the previous input source* under **System Settings →
Keyboard → Keyboard Shortcuts → Input Sources** to free `Ctrl+Space`.

## Supported agents

| Agent | Live status | Session resume | Precise events (hook) |
|---|:---:|:---:|:---:|
| Claude Code | ✓ | ✓ | ✓ |
| GitHub Copilot CLI | ✓ | ✓ | ✓ |
| Codex | ✓ | ✓ | ✓ |
| Antigravity CLI | ✓ | ✓ | session only |
| opencode | ✓ | ✓ | ✓ |
| Kimi | ✓ | ✓ | ✓ |
| Qoder CLI | ✓ | ✓ with integration | session only |
| Grok | ✓ | ✓ | ✓ |
| Hermes CLI | ✓ | ✓ with integration | session only |
| Pi | ✓ | ✓ | No |
| Oh My Pi (omp) | ✓ | ✓ | ✓ |
| Muse Code | ✓ | ✓ | No |
| Fx | ✓ | ✓ | No |
| Cursor | ✓ | resume command | No |
| Kilo Code | ✓ | exact-ID resume | No |
| Gemini · Aider · Amp · Droid · Qwen · Kiro | ✓ | No | No |

Live status needs no agent integration. See the
[documentation](https://luvus.dev/docs/) for setup, keybindings, modules, and
the complete CLI and API reference.

## Development

Read [CONTRIBUTING.md](CONTRIBUTING.md) for setup, tests, and pull request
requirements. Native agent contributors should also read
[Adding Agent Support](https://luvus.dev/docs/extend/adding-agent-support/).
Report vulnerabilities through [SECURITY.md](SECURITY.md).

## License

[Apache License 2.0](LICENSE).
