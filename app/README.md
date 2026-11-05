# Luvus Desktop

Development client for macOS arm64. The existing Luvus binary owns panes,
sessions, terminal history, and SSH connections. The Electron window attaches
to that binary over its App surface. Closing the window leaves the owner alive.

## Run locally

From the repository root, outside a managed Luvus pane:

```sh
npm --prefix app ci
npm --prefix app run dev
```

The development launcher builds the exact debug binary and prints its isolated
home under `target/`. It does not select an installed production session.
An explicitly launched desktop process also accepts `--session NAME` and
`--remote SSH_ALIAS`. Direct remote mode attaches through the core's SSH
bridge without starting a display-local server. Browser pages cannot choose
or replace the SSH destination.

A normal launch remembers the last successfully selected local session. On
first launch it tries `default`; an older running core is checked through its
read-only capabilities API before the binary desktop handshake. If it lacks
desktop support, the native dialog offers a separate session using the bundled
core. This choice leaves the original server and its terminals running and
does not migrate their workspaces. Cancel changes nothing; **Luvus > Retry
Connection** reopens recovery. Explicit `--session` and `--remote` still win
over the saved local choice.

## Desktop controls

The sidebar separates host-grouped workspaces and agents into independently
scrollable sections. Drag their divider to change the split. Agent state,
workspace scope, grouping (workspace, state, date or ungrouped), and section
size are local display preferences. The session button opens an on-demand
session picker; connection diagnostics remain under **Manage connections**.
Open Workspace offers the attached session and discovered sessions on registered hosts. The directory is resolved by the selected owner; Finder is only offered for local folders.

**New tab** and Command-T create a terminal immediately. Unnamed tabs display their one-based position; names set by a user or agent take precedence. Closing an individual pane takes effect immediately.
Both split buttons and the command-palette split action immediately request
the core's automatic split direction. Rename, move and transpose remain
separate actions. A new browser opens in a sibling core-owned tab, without
prompting for an address or creating a hidden PTY; switching to another tab preserves the live browser.

Settings has General, Appearance, Terminal, Workspaces, Agents, Keybindings,
Integrations and Advanced categories, plus cross-category search. General
offers a device interface language (English, Simplified Chinese or system default)
separately from the nine-language core selector. Appearance controls the desktop light/dark/system mode, interface
font/size, terminal theme and local background image. Terminal controls its
own font/size. Pick from installed font families, filtered to monospaced families for the terminal. Backgrounds accept PNG,
JPEG and WebP up to 20 MB and 32 megapixels, with visibility and fill/fit
controls; images stay in this device's Electron profile, not the owner config.

The terminal header exposes right-click passthrough and copy-on-selection
toggles for local and direct-SSH terminals. These are the core pane's persisted
mouse preferences, shared with TUI. Right-click opens the native terminal menu; Shift-right-click also opens it while a mouse-reporting program owns right-click. Registered projected terminals cannot yet
change them from the desktop. New browser-tab creation and mouse toggles need
the updated owner; an older running owner is never automatically restarted.

```sh
npm --prefix app run test:matrix -- desktop-ux layout-history local-sessions settings-inventory
```

## Browser checks

```sh
npm --prefix app run build
npm --prefix app run test:cdp
npm --prefix app run test:remote
```

Both checks launch Electron and an isolated owner, drive a browser leaf, and
print the screenshot directory. The remote check uses the repository's SSH
substitute with separate display and owner state, including a display-only
network trap. It does not start sshd or prove a connection to another computer.

The file-transfer checks exercise binary and empty uploads, traversal and
symlink rejection, owner-side PNG creation, downloads by name and by GUID,
existing destinations, and human takeover during a download. Download
permissions are temporary and tab-scoped. Unknown-length downloads are denied;
CDP callers must explicitly choose an existing directory on the owner.

For a two-machine check, explicitly choose a Linux SSH alias with an existing
checkout, Rust dependencies cached for a locked offline build, Python 3 and
Node. The following commands stage the current sources and fixture helpers
under that checkout's `target/`; they do not update the checkout, install
Luvus, change SSH configuration, or replace a running server:

```sh
node app/scripts/prepare-ssh-fixture.mjs HOST /absolute/path/to/luvus
npm --prefix app run test:ssh -- HOST /printed/target/app-e2e-real-XXXXXXXX
npm --prefix app run test:ssh:registered -- HOST /printed/target/app-e2e-real-XXXXXXXX
```

Use the fresh directory printed by the first command. The second starts the
isolated owner and two temporary loopback HTTP endpoints on that host. A native
Mac window attaches over real SSH, while Playwright runs on the Linux owner.
It checks terminal input, distinct same-port loopback pages, owner proxy
credentials, uploads, downloads, and detach. The test stops its services;
the build, screenshots, and file receipts remain available for inspection.

The registered-host check adds the fixture alias and enables same-name merge
through the GUI. It switches A/B/A on a shared SSH display, including an input
constructed before the switch and released after it. It interrupts only the
fixture display, then separately stops and restarts the marked fixture owner.
Assertions read both remote PTYs, check the old route is rejected in each
phase, and distinguish surviving PTYs from a new owner generation.
Append `--jump` to exercise two SSH jump hops through the approved host.
No system SSH settings or installed servers are changed.

## Test The Installed Package

```sh
npm --prefix app run pack:mac
# After installation from app/release/Luvus-<version>-mac-arm64.dmg:
npm --prefix app run test:installed -- --pressure
LUVUS_TEST_LEGACY_BINARY=/absolute/path/to/transport-20/luvus npm --prefix app run test:startup
LUVUS_TEST_BUNDLE=/Applications/Luvus.app npm --prefix app run test:matrix
LUVUS_TEST_BUNDLE=/Applications/Luvus.app npm --prefix app run test:ssh:registered -- HOST /printed/target/app-e2e-real-XXXXXXXX --jump
```

The smoke test starts the actual installed executable, checks the ASAR path
and embedded core bytes, and reopens the same isolated owner after quitting.
`--pressure` opens eight independent page guests and measures keyboard echo
while two screenshot requests are outstanding. Per-process resident memory
is not unique physical memory; core baseline measurements are separate.
The matrix writes `result.json` and per-case screenshots under the printed
fixture directory. It can run individual named cases without rebuilding.
The macOS package is local and ad-hoc signed; packaging does not publish it.
The startup regression uses the actual installed app with no session argument
beside an isolated, released transport-20 server. It checks cancel, separate
session recovery, remembered relaunch, and explicit session precedence while
preserving the old PID and generation. This case is required in addition to
the clean, explicitly named-session tests above.

The packaged Finder/Dock icon is generated at every package build from the
website's unchanged `website/public/brand/luvus-logo.png`, served at
<https://luvus.dev/brand/luvus-logo.png>.

## Scope And Limits

Run `npm --prefix app run parity:status` for the maintained inventory.
`test:parity` intentionally fails while entries remain open. A linked test
driver does not itself certify a release. See [acceptance evidence](ACCEPTANCE.md)
for the distinction between local, two-machine and instrumented checks.
