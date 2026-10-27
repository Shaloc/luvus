// Called only by the isolated SSH smoke fixture, using the production argv.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { readFile, writeFile, unlink } from "node:fs/promises";
import path from "node:path";
import WebSocket from "ws";
import { Cdp } from "./cdp.mjs";

const [executable, ...args] = process.argv.slice(2);
const profile = args.find(a => a.startsWith("--user-data-dir=")).split("=").slice(1).join("=");
assert.ok(profile.startsWith(path.join(process.env.LUVUS_SMOKE_ROOT, "local-state/browser-sessions/")));
const browser = spawn(executable, ["--headless=new", "--remote-debugging-port=0", "--disable-background-networking", "--disable-component-update", "--no-sandbox", ...args], { stdio: ["ignore", "ignore", "pipe"] });
let stderr = "", cdp;
browser.stderr.on("data", chunk => { stderr = (stderr + chunk).slice(-8192); });
const exited = new Promise(resolve => browser.once("exit", resolve));
async function until(check) {
  for (let i = 0; i < 200; i++) {
    const result = await check();
    if (result) return result;
    if (browser.exitCode !== null) throw new Error(`Chrome exited: ${stderr}`);
    await new Promise(resolve => setTimeout(resolve, 50));
  }
  throw new Error(`Chrome condition timed out: ${stderr}`);
}
try {
  const address = await until(async () => {
    try {
      const [port, endpoint] = (await readFile(path.join(profile, "DevToolsActivePort"), "utf8")).trim().split("\n");
      return `ws://127.0.0.1:${port}${endpoint}`;
    } catch { return undefined; }
  });
  const socket = new WebSocket(address);
  await new Promise((resolve, reject) => { socket.once("open", resolve); socket.once("error", reject); });
  cdp = new Cdp(socket);
  const { targetId } = await cdp.call("Target.createTarget", { url: "about:blank" });
  const { sessionId } = await cdp.call("Target.attachToTarget", { targetId, flatten: true });
  await cdp.call("Page.enable", {}, sessionId);
  let initialUrl = args.at(-1);
  if (initialUrl === "about:blank") {
    // Leave the browser blank first, then type a URL into this same session.
    const { targetInfos } = await cdp.call("Target.getTargets");
    assert.ok(targetInfos.some(target => target.type === "page" && target.url === "about:blank"));
    const [port] = JSON.parse(await readFile(path.join(process.env.LUVUS_SMOKE_ROOT, "browser-http.json"), "utf8"));
    initialUrl = `http://localhost:${port}/from-blank`;
  }
  const result = await cdp.call("Page.navigate", { url: initialUrl }, sessionId);
  assert.equal(result.errorText, undefined, JSON.stringify(result));
  const read = async () => (await cdp.call("Runtime.evaluate", { expression: "document.body?.textContent", returnByValue: true }, sessionId)).result.value;
  const marker = process.env.LUVUS_TEST_BROWSER_OWNER === "local" ? "LOCAL_BROWSER_MARKER" : "REMOTE_BROWSER_MARKER";
  await until(async () => (await read())?.includes(marker));
  if (process.env.LUVUS_TEST_BROWSER_PROXY) {
    const result = await cdp.call("Page.navigate", { url: "http://proxy-only.invalid/from-chrome" }, sessionId);
    assert.equal(result.errorText, undefined);
    await until(async () => (await read())?.includes(marker.startsWith("REMOTE") ? "remote-http" : "local-http"));
    // Trust only the isolated self-signed fixture via CDP, never production argv.
    await cdp.call("Security.setIgnoreCertificateErrors", { ignore: true }, sessionId);
    const tlsPort = JSON.parse(await readFile(path.join(process.env.LUVUS_SMOKE_ROOT, "browser-https.json"), "utf8"));
    const secure = await cdp.call("Page.navigate", { url: `https://https-proxy-only.invalid:${tlsPort}/tls` }, sessionId);
    assert.equal(secure.errorText, undefined);
    await until(async () => (await read())?.includes("HTTPS_BROWSER_MARKER"));
  }
  if (marker.startsWith("REMOTE")) {
    const url = new URL(initialUrl);
    url.hostname = "remote-only.invalid";
    const result = await cdp.call("Page.navigate", { url: url.toString() }, sessionId);
    assert.equal(result.errorText, undefined);
    await until(async () => (await read())?.includes(marker));
    // The same local HTTP server stays reachable. A proxy failure must not
    // silently navigate to it instead of the owner's localhost.
    const blocked = path.join(process.env.LUVUS_SMOKE_ROOT, "browser-proxy-drop");
    await writeFile(blocked, "");
    try {
      url.hostname = "127.0.0.1";
      const failed = await cdp.call("Page.navigate", { url: url.toString() }, sessionId);
      if (process.env.LUVUS_TEST_BROWSER_PROXY) {
        assert.ok(failed.errorText || (await read())?.includes("Owner browser proxy connection failed"));
      } else assert.ok(failed.errorText, "proxy failure must fail the navigation");
      assert.ok(!(await read())?.includes("LOCAL_BROWSER_MARKER"));
    } finally { await unlink(blocked); }
  }
  console.log(marker);
} finally {
  if (cdp) await cdp.call("Browser.close").catch(() => browser.kill());
  else browser.kill();
  await exited;
  cdp?.socket.close();
}
