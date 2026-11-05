# Desktop acceptance

Scope: local macOS arm64 installation and a Linux x86_64 SSH owner. Tests
use private homes (including an isolated default session). No production server, global clipboard, agent
credentials, system SSH configuration or network interface is a test fixture.
VoiceOver and accessibility certification are excluded by request.

| Boundary | Runtime evidence |
| --- | --- |
| Desktop interaction | `desktop-ux`: fixed 6px agent status dots at wide/compact widths, independently sized sidebar sections, styled filter popup, on-demand session picker without leaving the terminal, independent footer hover targets, desktop language switching, immediate numbered tabs and automatic splits, core mouse toggles, live fonts and device-local wallpaper restored after reload. Browser checks switch between terminal and browser sibling tabs, with the document title reflected in the tab strip. |
| Registered owner | `test:ssh:registered -- HOST ROOT --jump` opens a real installed window. The owner shells are queried independently after A/B/A, transport loss and owner restart; a delayed input must be rejected and absent on both. |
| Direct owner network | `test:ssh` verifies authenticated owner HTTP proxy, NO_PROXY localhost, WebSocket, binary upload/download and detach. Stopping just the test proxy returns an error despite valid same-port direct destinations on both machines. With a pinned owner-side agent-browser supplied to the fixture preparer, it also runs agent-browser on Linux. |
| Display network probes | A page attempts ICE with a UDP listener on the display and a unique DNS probe. The run records ICE gathering state, candidate count, display packets, and Chromium resolver events for that name. It does not change Wi-Fi, DNS settings or firewall rules. |
| Local TLS and IPv6 | The guest reaches a real IPv6 loopback service and rejects an untrusted certificate. A test-only verifier trusts exactly one generated leaf for localhost in that guest. Two HTTPS requests observe the server's HTTP/3 Alt-Svc advertisement; NetLog and a UDP listener check for QUIC activity. No system trust is modified. |
| Terminal and layout | `layout-history`, `workspace-lifecycle`, `terminal-ime`, `terminal-selection`, `terminal-graphics`: real PTYs, Chromium input, Kitty pixel checks, soft-wrap copy, stale geometry, background workspaces and zero workspace. Clipboard writes are captured at the fixture boundary. |
| Concurrent clients | `commander-private`, `commander-receipts`, `mixed-clients`, `slow-display`: separate drafts and receipts, private scroll/copy, shared notices, read-only Web, and a deliberately paused display that catches up to live output. |
| Review and files | `files`, `native-diff`, `github`: real files, Git, Markdown/Mermaid and split diff. Stale rename/delete/edit preserve newer data. GitHub responses are from a private gh executable, including missing/offline cases. |
| Agent lifecycle | `agent-native`, `agent-recovery`, `active-automation`: the real native-store adapter reads a synthetic rollout. An instrumented offline executable receives actual resume/fork argv and PTY prompts. Restart waits for fresh process evidence, and a different native session in the same directory cannot take over the binding. No hosted agent request or real token spend is implied. |
| Operator workflows | `orchestration`, `module-widgets`, `settings-modules`, `settings-inventory`, `diagnostics`: real worktree and quality gate, module subprocess actions, config changes and local integration management, loopback bridge start/revocation and bounded waits. |
| Browser tools | `test:cdp` and `test:remote` cover scoped Playwright, an authorized cross-origin child, rejected unrelated targets, staging/overwrite, cancellation, human takeover and sandboxing. The save test uses an actual Chromium download and verifies native chooser options and exact bytes; only the destination selection is substituted. |
| Installed binary | `test:installed -- --pressure` records the executable/ASAR identity, owner generation, three window sizes, Retina scale and eight guest processes. The app and core are not automatically notarized or published. |
| Cold launch beside a released core | `test:startup` starts a released transport-20 owner in a private home, then opens the actual bundle without `--session`. It verifies compatibility detection, cancel, explicit recovery to a new session, working PTY input, remembered relaunch, and explicit session precedence. The old PID/generation must stay unchanged. A same-version, freshly started test owner does not cover this boundary. |

## Outstanding

- The desktop does not yet offer one-use graphical SSH authentication or host-key admission. Real SSH checks use pre-authorized keys; no test adds a trust exception.
- Registered terminals have verified routing. Registered browser leaves stay unavailable until owner-scoped browser handover has a complete implementation and a real two-machine test. Direct SSH browser results cannot stand in for registered handover.
- Sensitive site permissions remain denied by default. Human download byte/chooser-option checks do not certify every physical macOS permission or file dialog.
- The tested Linux owner has no IPv6 socket support (`EAFNOSUPPORT`). A positive remote IPv6 test and parity of the entire proxy/TLS matrix on that owner remain open. The positive IPv6 and pinned HTTPS/QUIC checks above run on the Mac. A zero-packet probe is evidence for that probe only.
- The core's language preference persists across all nine supported values.
  The separate desktop language switch supports English, Simplified Chinese and system default; other desktop UI locales are not offered.
- The new browser-tab and per-pane mouse-preference requests require an updated
  owner. Attaching to an older compatible owner does not restart or upgrade it.
  Registered projected terminals still lack desktop mouse-preference controls.

For the machine-readable surface inventory see [feature-parity.json](tests/feature-parity.json).
Inspect the printed fixture results alongside it; a static count of linked
scenarios is not a substitute for an executable test.
