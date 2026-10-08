import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { mkdir, mkdtemp, rm, readFile, readdir } from "node:fs/promises";
import path from "node:path";
import readline from "node:readline";
import { fileURLToPath } from "node:url";
import WebSocket from "ws";

const webRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const repoRoot = path.resolve(webRoot, "..");
const executable = path.resolve(process.env.LUVUS_BIN
  || path.join(repoRoot, "target", "debug", process.platform === "win32" ? "luvus.exe" : "luvus"));
await mkdir(path.join(repoRoot, "target"), { recursive: true });
const remoteFixture = process.env.LUVUS_WEB_REMOTE_FIXTURE;
if (remoteFixture) {
  assert.equal(path.dirname(remoteFixture), path.join(repoRoot, "target"));
  assert.ok(path.basename(remoteFixture).startsWith("remote-smoke-"));
  assert.equal(await readFile(path.join(remoteFixture, ".isolated-smoke"), "utf8"), remoteFixture);
  assert.ok(executable.startsWith(path.join(repoRoot, "target") + path.sep));
}
const home = remoteFixture ? path.join(remoteFixture, "local-state")
  : await mkdtemp(path.join(repoRoot, "target", "luvus-native-web-"));
const workspace = path.join(home, "workspace");
await mkdir(workspace);
const session = remoteFixture ? "api" : "native-web-" + process.pid;
const env = { ...Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith("LUVUS_"))), LUVUS_HOME: home };
delete env.LUVUS_SOCKET_PATH;
delete env.LUVUS_SESSION;
delete env.LUVUS_BIN_PATH;
if (remoteFixture) {
  env.LUVUS_SMOKE_ROOT = remoteFixture;
  env.LUVUS_SMOKE_BINARY = executable;
  env.LUVUS_SMOKE_REMOTE_BINARY = executable;
}
let child;
let stderr = "";
const sockets = new Set();

try {
  await (remoteFixture ? testRemote() : testLocal());
} finally {
  for (const socket of sockets) socket.terminate();
  if (child?.exitCode === null) child.kill("SIGINT");
  if (!remoteFixture) {
    run(["--session", session, "server", "stop"], env, true);
    run(["session", "delete", session], env, true);
    await rm(home, { recursive: true, force: true });
  }
}

async function testLocal() {
  const paired = await startBridge();
  const code = paired.hash.startsWith("#pair=") ? decodeURIComponent(paired.hash.slice(6)) : "";
  assert.ok(code);

  const { socket, ready } = await authenticate(paired, { code });
  assert.equal(ready.authority.mode, "control");
  assert.equal(ready.expires_on_close, true);
  assert.equal(ready.expires_at, undefined);
  assert.ok(ready.ticket);
  const second = await authenticate(paired, { ticket: ready.ticket });
  assert.equal(second.ready.expires_on_close, true);
  assert.equal(second.ready.ticket, undefined, "another tab reuses the same grant");
  assert.equal((await request(second.socket, "tab-devices", "web.devices.status", {})).paired_devices, 1);
  await request(second.socket, "tab-snapshot", "session.snapshot", {});
  await rejected(paired, { code }); // Spent links still cannot pair a different browser.

  const capabilities = await request(socket, "capabilities", "uhp.capabilities", {});
  assert.equal(capabilities.type, "uhp_capabilities");
  assert.ok(capabilities.access.allowed_methods.includes("terminal.backend.control"));
  assert.ok(capabilities.terminal.capabilities.includes("set_viewport"));
  assert.equal(capabilities.terminal.limits.viewport_cols, 500);
  assert.equal(capabilities.terminal.limits.viewport_rows, 300);

  const deviceStatus = await request(socket, "device-status", "web.devices.status", {});
  assert.equal(deviceStatus.public_url, null);
  const publicStatus = await request(socket, "public-url", "web.devices.set_public_url", {
    url: "https://phone.example",
  });
  assert.equal(publicStatus.public_url, "https://phone.example");
  const phonePairing = await request(socket, "phone-pairing", "web.devices.create_pairing", {});
  assert.equal(phonePairing.url, `https://phone.example/#pair=${encodeURIComponent(phonePairing.code)}`);
  const phone = await authenticate(paired, { code: phonePairing.code });
  assert.notEqual(phone.ready.ticket, ready.ticket);
  assert.equal((await request(phone.socket, "phone-devices", "web.devices.status", {})).paired_devices, 2);

  const snapshot = await request(socket, "snapshot", "session.snapshot", {});
  const pane = snapshot.workspaces.flatMap((workspace) => workspace.tabs)
    .flatMap((tab) => tab.panes)
    .find((candidate) => candidate.kind === "terminal" && candidate.terminal_id);
  assert.ok(pane);

  const initialFrame = waitFor(socket, (frame) => frame.type === "stream.frame"
    && frame.stream_id === "control" && frame.frame?.event === "terminal.frame");
  const controlAck = waitFor(socket, (frame) => frame.type === "response" && frame.id === "control");
  socket.send(JSON.stringify({
    type: "stream.open",
    id: "control",
    method: "terminal.backend.control",
    params: {
      server_generation: snapshot.server_generation,
      terminal_id: pane.terminal_id,
      pane_id: pane.pane_id,
      mode: "recent_unwrapped",
      lines: 80,
      ansi: true,
      cursor: true,
    },
  }));
  assert.equal((await controlAck).result.type, "terminal_backend_stream");
  const revision = (await initialFrame).frame.data.content_revision;
  const reflow = waitFor(socket, (frame) => frame.type === "stream.frame"
    && frame.stream_id === "control" && frame.frame?.event === "terminal.frame"
    && frame.frame.data.content_revision > revision);
  const resized = await streamAction(socket, "viewport", "set_viewport", { cols: 40, rows: 18 });
  assert.equal(resized.result?.dispatch, "executed");
  await reflow;
  if (process.platform !== "win32") {
    const sizeMarker = `VIEWPORT_${process.pid}`;
    const sizeOutput = waitFor(socket, (frame) => frame.type === "stream.frame"
      && frame.stream_id === "control" && frame.frame?.data?.text?.includes(`${sizeMarker} 18 40`), 10_000);
    const sizeAction = await streamAction(socket, "viewport-size", "submit_text", {
      text: `printf '\\n${sizeMarker} '; stty size`,
    });
    assert.equal(sizeAction.result?.type, "terminal_backend_action");
    await sizeOutput;
  }
  const marker = "NATIVE_WEB_" + process.pid;
  const output = waitFor(socket, (frame) => frame.type === "stream.frame"
    && frame.stream_id === "control"
    && frame.frame?.event === "terminal.frame"
    && frame.frame.data?.text?.includes(marker), 10_000);
  const action = waitFor(socket, (frame) => frame.type === "response" && frame.id === "action");
  socket.send(JSON.stringify({
    type: "stream.action",
    stream_id: "control",
    id: "action",
    action: "submit_text",
    params: { text: "printf '" + marker + "\\n'" },
  }));
  assert.equal((await action).result.type, "terminal_backend_action");
  await output;

  // Exercise the interactive-input budget without overflowing the intentionally
  // tiny terminal observation queue on a slow CI runner. Each action must
  // receive its own successful response before the next is sent.
  const burstCount = 180;
  const burstStarted = performance.now();
  for (let index = 0; index < burstCount; index += 1) {
    const id = `burst-${index}`;
    const response = waitForCount(socket, (frame) => frame.type === "response"
      && frame.id === id, 1, 10_000);
    socket.send(JSON.stringify({
      type: "stream.action",
      stream_id: "control",
      id,
      action: "send_key",
      params: { key: index % 2 === 0 ? "left" : "right" },
    }));
    const [result] = await response;
    assert.equal(result.result?.type, "terminal_backend_action",
      `terminal input ${id} failed: ${JSON.stringify(result)}`);
  }
  const burstElapsed = performance.now() - burstStarted;
  // The gateway intentionally closes a stream on malformed input; it must
  // never forward an out-of-bounds resize or revoke the browser's authority.
  const invalidClosed = waitFor(socket, (frame) => frame.type === "stream.closed"
    && frame.stream_id === "control");
  socket.send(JSON.stringify({ type: "stream.action", stream_id: "control", id: "invalid-viewport",
    action: "set_viewport", params: { cols: 501, rows: 18 } }));
  await invalidClosed;
  await request(socket, "after-invalid-viewport", "session.snapshot", {});
  const tabsClosed = Promise.all([closed(socket), closed(second.socket)]);
  socket.close();
  second.socket.close();
  await tabsClosed;
  const reopened = await authenticate(paired, { ticket: ready.ticket });
  const reopenedTab = await authenticate(paired, { ticket: ready.ticket });
  await request(reopened.socket, "reopened-snapshot", "session.snapshot", {});
  assert.equal((await request(reopened.socket, "reopened-devices", "web.devices.status", {})).paired_devices, 2);
  await assert.rejects(request(reopened.socket, "foreign-forget", "web.devices.forget", { ticket: phone.ready.ticket }), /method_not_found/);
  const forgottenTabs = Promise.all([closed(reopened.socket), closed(reopenedTab.socket)]);
  const forgotten = await request(reopened.socket, "forget", "web.devices.forget", {});
  assert.equal(forgotten.type, "browser_device_forgotten");
  await forgottenTabs;
  await rejected(paired, { ticket: ready.ticket });
  assert.equal((await request(phone.socket, "remaining-devices", "web.devices.status", {})).paired_devices, 1);
  await request(phone.socket, "still-live", "session.snapshot", {});

  const phoneClosed = closed(phone.socket);
  child.kill("SIGINT");
  await exited(child, 10_000);
  await phoneClosed;
  child = undefined;
  const status = run(["--session", session, "server", "status"], env);
  assert.equal(status.status, 0, status.stderr);
  assert.match(status.stdout, /running/);
  const finite = await startBridge(["--ticket-ttl", "2"]);
  await rejected(finite, { ticket: phone.ready.ticket }); // Stopping a bridge revoked all its tickets.
  const limited = await authenticate(finite, { code: finite.hash.slice(6) });
  assert.equal(typeof limited.ready.expires_at, "number");
  assert.equal(limited.ready.expires_on_close, undefined);
  await closed(limited.socket);
  await rejected(finite, { ticket: limited.ready.ticket });
  child.kill("SIGINT");
  await exited(child, 10_000);
  child = undefined;
  process.stdout.write(`native luvus web integration passed (viewport, validation, reflow, multi-tab, reconnect, revoke, finite expiry, shutdown; ${burstCount} inputs in ${Math.round(burstElapsed)}ms)\n`);
}

async function startBridge(extra = []) {
  stderr = "";
  child = spawn(executable, [
    "--session", session, "web", ...(extra.includes("--read-only") ? [] : ["--control"]), "--port", "0", "--no-open", ...extra,
  ], { cwd: workspace, env, stdio: ["ignore", "pipe", "pipe"], windowsHide: true });
  child.stderr.setEncoding("utf8");
  child.stderr.on("data", (chunk) => { stderr = (stderr + chunk).slice(-8192); });
  const lines = readline.createInterface({ input: child.stdout });
  return new URL(await waitForLine(lines, (line) => line.startsWith("http://"), 20_000));
}

async function authenticate(address, credential) {
  const socket = new WebSocket(address.origin.replace(/^http/, "ws") + "/bridge", { origin: address.origin });
  sockets.add(socket);
  socket.once("close", () => sockets.delete(socket));
  await opened(socket);
  const response = waitFor(socket, (frame) => frame.type === "ready" || frame.type === "error");
  socket.send(JSON.stringify({ type: "authenticate", ...credential }));
  return { socket, ready: await response };
}

async function rejected(address, credential) {
  const { socket, ready } = await authenticate(address, credential);
  assert.equal(ready.type, "error");
  assert.equal(ready.code, "forbidden");
  await closed(socket);
}

function closed(socket) {
  if (socket.readyState === WebSocket.CLOSED) return Promise.resolve();
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      socket.off("close", finish);
      reject(new Error("WebSocket did not close"));
    }, 5_000);
    const finish = () => { clearTimeout(timer); resolve(); };
    socket.once("close", finish);
  });
}

function run(args, runEnv, allowFailure = false) {
  const result = spawnSync(executable, args, {
    cwd: workspace,
    env: runEnv,
    encoding: "utf8",
    timeout: 20_000,
    windowsHide: true,
  });
  if (!allowFailure && (result.error || result.status !== 0)) {
    throw result.error || new Error(result.stderr || "luvus exited " + result.status);
  }
  return result;
}

function waitForLine(lines, predicate, timeoutMs) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => finish(new Error("native web startup timed out: " + stderr)), timeoutMs);
    const onLine = (line) => {
      if (predicate(line)) finish(undefined, line);
    };
    const onClose = () => finish(new Error("native web exited before startup: " + stderr));
    const finish = (error, value) => {
      clearTimeout(timer);
      lines.off("line", onLine);
      lines.off("close", onClose);
      if (error) reject(error); else resolve(value);
    };
    lines.on("line", onLine);
    lines.on("close", onClose);
  });
}

function opened(socket) {
  return new Promise((resolve, reject) => {
    socket.once("open", resolve);
    socket.once("error", reject);
  });
}

function request(socket, id, method, params) {
  const response = waitFor(socket, (frame) => frame.type === "response" && frame.id === id);
  socket.send(JSON.stringify({ type: "request", id, method, params }));
  return response.then((frame) => {
    if (frame.error) throw new Error(frame.error.code + ": " + frame.error.message);
    return frame.result;
  });
}

function streamAction(socket, id, action, params) {
  const response = waitFor(socket, (frame) => frame.type === "response" && frame.id === id);
  socket.send(JSON.stringify({ type: "stream.action", stream_id: "control", id, action, params }));
  return response;
}

function waitFor(socket, predicate, timeoutMs = 5_000) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => finish(new Error("WebSocket response timed out: " + predicate.toString())), timeoutMs);
    const onMessage = (data) => {
      let frame;
      try { frame = JSON.parse(data.toString()); } catch { return; }
      if (predicate(frame)) finish(undefined, frame);
    };
    const onClose = () => finish(new Error("WebSocket closed"));
    const finish = (error, value) => {
      clearTimeout(timer);
      socket.off("message", onMessage);
      socket.off("close", onClose);
      if (error) reject(error); else resolve(value);
    };
    socket.on("message", onMessage);
    socket.on("close", onClose);
  });
}

function waitForCount(socket, predicate, count, timeoutMs) {
  return new Promise((resolve, reject) => {
    const frames = [];
    const timer = setTimeout(() => finish(new Error(`WebSocket response window timed out (${frames.length}/${count})`)), timeoutMs);
    const onMessage = (data) => {
      let frame;
      try { frame = JSON.parse(data.toString()); } catch { return; }
      if (frame.type === "stream.closed" && frame.stream_id === "control") {
        finish(new Error(`Terminal control stream closed during response window: ${JSON.stringify(frame)}`));
        return;
      }
      if (!predicate(frame)) return;
      frames.push(frame);
      if (frames.length === count) finish(undefined, frames);
    };
    const onClose = () => finish(new Error("WebSocket closed during response window"));
    const finish = (error, value) => {
      clearTimeout(timer);
      socket.off("message", onMessage);
      socket.off("close", onClose);
      if (error) reject(error); else resolve(value);
    };
    socket.on("message", onMessage);
    socket.on("close", onClose);
  });
}

function exited(process, timeoutMs) {
  return new Promise((resolve, reject) => {
    if (process.exitCode !== null) return resolve();
    const timer = setTimeout(() => reject(new Error("native web did not stop")), timeoutMs);
    process.once("exit", () => {
      clearTimeout(timer);
      resolve();
    });
  });
}


// Reuses the real SSH-owner fixture from scripts/test-remote-sessions.py --web-only.
async function testRemote() {
  const paired = await startBridge();
  const { socket } = await authenticate(paired, { code: paired.hash.slice(6) });
  let serial = 0;
  const snapshot = () => request(socket, `snapshot-${serial++}`, "session.snapshot", {});
  const allPanes = (s) => s.workspaces.flatMap(w => w.tabs.flatMap(t => t.panes));
  const remotePane = (s) => s.workspaces.filter(w => w.host === "fake-dev").flatMap(w => w.tabs.flatMap(t => t.panes)).find(p => p.terminal_id);
  const native = (method, params = {}, remote = false) => {
    const runEnv = remote ? { ...env, HOME: path.join(remoteFixture, "remote-home"), LUVUS_HOME: path.join(remoteFixture, "remote-state") } : env;
    const result = spawnSync(executable, ["--session", "api", "uhp", "proxy"], {
      cwd: workspace, env: runEnv, encoding: "utf8", timeout: 20_000,
      input: JSON.stringify({ id: "fixture", method, params }) + "\n",
    });
    assert.equal(result.status, 0, result.stderr);
    const response = JSON.parse(result.stdout);
    assert.ok(response.result, JSON.stringify(response));
    return response.result;
  };
  const original = await snapshot();
  const pane = remotePane(original);
  assert.ok(pane, JSON.stringify(original));
  const local = original.workspaces.filter(w => !w.host).flatMap(w => w.tabs.flatMap(t => t.panes)).find(p => p.terminal_id);
  assert.ok(local);
  assert.notEqual(local.pane_id, pane.pane_id);
  const remoteNative = allPanes(native("session.snapshot", {}, true)).find(p => p.terminal_id);
  assert.equal(pane.display_pane_id, remoteNative.pane_id);
  assert.equal(local.pane_id, remoteNative.pane_id, "fixture exercises colliding owner-local pane IDs");
  const target = { server_generation: original.server_generation, terminal_id: pane.terminal_id,
    pane_id: pane.pane_id, mode: "recent_unwrapped", lines: 80, ansi: false };
  async function open(ws, id, params, control = true) {
    const ack = waitFor(ws, f => f.type === "response" && f.id === id, 20_000);
    ws.send(JSON.stringify({ type: "stream.open", id, method: `terminal.backend.${control ? "control" : "observe"}`, params }));
    return ack;
  }
  const first = waitFor(socket, f => f.stream_id === "control" && f.frame?.event === "terminal.frame", 20_000);
  assert.equal((await open(socket, "control", target)).result.type, "terminal_backend_stream");
  assert.equal((await first).frame.data.pane_id, pane.pane_id);
  assert.equal((await streamAction(socket, "resize", "set_viewport", {cols: 71, rows: 21})).result.dispatch, "executed");
  const marker = `WEB_REMOTE_${process.pid}`;
  const output = waitFor(socket, f => f.frame?.data?.text?.includes(`${marker} 21 71`), 15_000);
  await streamAction(socket, "remote-command", "submit_text", {
    text: `printf '${marker} '; stty size; printf '${marker}' > "$HOME/web-owner-proof"`,
  });
  await output;
  assert.equal(await readFile(path.join(remoteFixture, "remote-home/web-owner-proof"), "utf8"), marker);
  await assert.rejects(readFile(path.join(remoteFixture, "local-home/web-owner-proof")));
  assert.ok(!JSON.stringify(native("pane.read", { pane: local.pane_id })).includes(marker));

  const pasted = waitFor(socket, f => f.frame?.data?.text?.includes("PASTE_OK_" + marker), 10_000);
  await streamAction(socket, "literal", "type_literal", {text:"printf "});
  await streamAction(socket, "paste", "paste_text", {text:"'PASTE_OK_" + marker + "\\n'"});
  await streamAction(socket, "enter", "send_key", {key:"enter"});
  await pasted;

  const upload = (await streamAction(socket, "upload-start", "upload_start", {name: "remote proof.txt", size: 8})).result;
  assert.equal(upload.type, "terminal_upload");
  assert.equal((await streamAction(socket, "upload-chunk", "upload_chunk", {
    upload_id: upload.upload_id, offset: 0, data_base64: "d2ViIGZpbGU=",
  })).result.received, 8);
  assert.equal((await streamAction(socket, "upload-finish", "upload_finish", {upload_id: upload.upload_id})).result.type, "terminal_backend_action");
  const files = await readdir(path.join(remoteFixture, "remote-state"), {recursive: true});
  const uploaded = files.find(f => f.endsWith("remote_proof.txt"));
  assert.ok(uploaded, JSON.stringify(files));
  assert.equal(await readFile(path.join(remoteFixture, "remote-state", uploaded), "utf8"), "web file");
  assert.ok(!(await readdir(home, {recursive: true})).some(f => f.endsWith("remote_proof.txt")));
  await streamAction(socket, "clear-upload", "send_key", {key: "ctrl-c"});

  // The existing topology feed must carry agent/title changes to Web without polling SSH.
  const eventAck = waitFor(socket, f => f.type === "response" && f.id === "events");
  socket.send(JSON.stringify({type:"stream.open", id:"events", method:"events.subscribe", params:{}}));
  await eventAck;
  const fixtureReady = waitFor(socket, f => f.frame?.data?.text?.includes("CODEX_FIXTURE_READY"), 15_000);
  const quote = text => "'" + text.replaceAll("'", "'\\''") + "'";
  const command = "exec -a codex " + quote(process.env.LUVUS_WEB_REMOTE_PYTHON) + " " + quote(path.join(repoRoot, "scripts/test-remote-sessions.py")) + " --codex-fixture";
  await streamAction(socket, "start-agent", "submit_text", {text:"bash -c " + quote(command)});
  await fixtureReady;
  const changed = waitFor(socket, f => f.stream_id === "events" && f.frame?.event === "workspace.metadata_reported", 15_000);
  await streamAction(socket, "status-report", "submit_text", {
    text: "OSC7501:state=blocked:app=codex:msg=V2ViIHJlbW90ZQ==",
  });
  await changed;
  let current;
  for (let n = 0; n < 30; n++) {
    current = await snapshot();
    if (remotePane(current)?.agent_status === "blocked") break;
    await new Promise(resolve => setTimeout(resolve, 100));
  }
  assert.equal(remotePane(current)?.agent_status, "blocked");
  assert.equal(remotePane(current)?.is_agent, true);

  const renamed = waitFor(socket, f => f.stream_id === "events" && f.frame?.event === "workspace.metadata_reported", 15_000);
  native("pane.rename", {pane:remoteNative.pane_id, name:"web-owner-agent"}, true);
  await renamed;
  for (let n=0; n<30; n++) {
    current = await snapshot();
    if (remotePane(current)?.agent_name === "web-owner-agent") break;
    await new Promise(resolve => setTimeout(resolve, 100));
  }
  assert.equal(remotePane(current)?.agent_name, "web-owner-agent");

  // A forged same-host ID cannot land on a local pane or a different remote.
  assert.equal((await open(socket, "forged", {...target, pane_id: local.pane_id})).error.code, "stale_terminal");
  socket.send(JSON.stringify({type:"stream.close", stream_id:"control"}));
  socket.close();
  await closed(socket);
  child.kill("SIGINT"); await exited(child, 10_000); child = undefined;

  const ro = await startBridge(["--read-only"]);
  const readOnly = (await authenticate(ro, {code:ro.hash.slice(6)})).socket;
  assert.equal((await open(readOnly, "denied", target)).error.code, "forbidden");
  const roFrame = waitFor(readOnly, f => f.stream_id === "observe" && f.frame?.event === "terminal.frame", 15_000);
  assert.equal((await open(readOnly, "observe", target, false)).result.type, "terminal_backend_stream");
  await roFrame;
  const actionReply = waitFor(readOnly, f => f.type === "stream.closed" && f.stream_id === "observe");
  readOnly.send(JSON.stringify({type:"stream.action",stream_id:"observe",id:"ro-input",action:"submit_text",params:{text:`touch ${remoteFixture}/readonly-must-not-exist`}}));
  await actionReply;
  readOnly.close(); await closed(readOnly);
  child.kill("SIGINT"); await exited(child, 10_000); child = undefined;

  // Offline owners keep their labels, but cannot accept stale terminal input.
  const offlineUrl = await startBridge();
  const offline = (await authenticate(offlineUrl, {code:offlineUrl.hash.slice(6)})).socket;
  run(["--session","api","server","stop"], {...env, HOME:path.join(remoteFixture,"remote-home"), LUVUS_HOME:path.join(remoteFixture,"remote-state")});
  let disconnected;
  for (let n=0; n<40; n++) {
    disconnected = await request(offline, `offline-${n}`, "session.snapshot", {});
    if (!remotePane(disconnected)) break;
    await new Promise(resolve => setTimeout(resolve, 150));
  }
  assert.ok(!remotePane(disconnected));
  assert.ok(disconnected.workspaces.some(w => w.host === "fake-dev" && w.name.includes("offline")));
  assert.equal((await open(offline, "offline-owner", target)).error.code, "stale_terminal");
  assert.equal((await open(offline, "local-live", {...target,pane_id:local.pane_id,terminal_id:local.terminal_id})).result.type, "terminal_backend_stream");
  offline.close(); await closed(offline);
  child.kill("SIGINT"); await exited(child, 10_000); child = undefined;

  // Restart only the isolated owner: old routes fail closed even if pane IDs recur.
  const restarted = await startBridge();
  const again = (await authenticate(restarted, {code:restarted.hash.slice(6)})).socket;
  run(["--session","api","server","restart"], {...env, HOME:path.join(remoteFixture,"remote-home"), LUVUS_HOME:path.join(remoteFixture,"remote-state")});
  let fresh;
  for (let n=0; n<50; n++) {
    fresh = await request(again, `restart-snapshot-${n}`, "session.snapshot", {});
    if (remotePane(fresh)?.terminal_id !== target.terminal_id && remotePane(fresh)) break;
    await new Promise(resolve => setTimeout(resolve, 150));
  }
  assert.ok(remotePane(fresh));
  assert.notEqual(remotePane(fresh).terminal_id, target.terminal_id);
  assert.equal((await open(again, "old-owner", target)).error.code, "stale_terminal");
  const live = remotePane(fresh);
  const recovered = await open(again, "recovered", {...target, pane_id:live.pane_id, terminal_id:live.terminal_id});
  assert.equal(recovered.result.type, "terminal_backend_stream");
  run(["--session", "web-switch", "server", "start"], env);
  const switchLink = await request(again, "switch-link", "web.devices.create_pairing", {});
  const switcher = (await authenticate(restarted, {code:switchLink.code})).socket;
  const oldStreamClosed = waitFor(again, f => f.type === "stream.closed" && f.stream_id === "recovered", 10_000);
  await request(switcher, "switch-session", "web.sessions.switch", {name:"web-switch"});
  await oldStreamClosed;
  assert.equal((await open(again, "old-session", {...target,pane_id:live.pane_id,terminal_id:live.terminal_id})).error.code, "stale_server");
  await request(switcher, "switch-back", "web.sessions.switch", {name:"api"});
  const shutdownSnapshot = await request(again, "shutdown-snapshot", "session.snapshot", {});
  const shutdownPane = remotePane(shutdownSnapshot);
  assert.ok(shutdownPane);
  assert.equal((await open(again, "shutdown-stream", {...target, server_generation:shutdownSnapshot.server_generation,
    pane_id:shutdownPane.pane_id, terminal_id:shutdownPane.terminal_id})).result.type, "terminal_backend_stream");
  const shutdownClients = Promise.all([closed(again), closed(switcher)]);
  child.kill("SIGINT"); await exited(child, 10_000); child = undefined;
  await shutdownClients;
  assert.equal(run(["--session","api","server","status"], env).status, 0);
  native("session.snapshot", {}, true);
  console.log("PASS: native Web remote snapshot, namespaces, status events, PTY input/resize, owner-side upload, read-only, offline/local independence, restart fencing, cross-device session switch and bridge cleanup");
}
