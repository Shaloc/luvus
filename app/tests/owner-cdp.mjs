// Executed on the explicitly selected SSH test owner, not on the display.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { readFile, writeFile } from 'node:fs/promises';
import { createInterface } from 'node:readline';
import { resolve } from 'node:path';
import { chromium } from 'playwright-core';
import { existsSync } from 'node:fs';

const gateway = spawn(process.env.LUVUS_APP_BINARY, ['--session', 'ssh', 'browser', 'cdp', '--browser',
  process.argv[2], '--ttl', '90', '--json'], { stdio: ['ignore', 'pipe', 'inherit'] });
const lines = createInterface({ input: gateway.stdout });
const exited = once(gateway, 'exit');
let client,report;
try {
  const [line] = await Promise.race([once(lines, 'line', { signal: AbortSignal.timeout(20_000) }),
    exited.then(() => { throw new Error('CDP gateway closed during startup'); })]);
  client = await chromium.connectOverCDP(JSON.parse(line).endpoint, { noDefaults: true, timeout: 15_000 });
  const context = client.contexts()[0];
  assert.equal(context.pages().length, 1);
  const page = context.pages()[0];
  assert.equal(await page.title(), 'OWNER LOCALHOST');
  const api = await context.newCDPSession(page);
  const { root } = await api.send('DOM.getDocument');
  const { nodeId } = await api.send('DOM.querySelector', { nodeId: root.nodeId, selector: '#upload' });
  const bytes = Buffer.from(Array.from({ length: 19_227 }, (_, i) => (i * 13) % 256));
  const path = resolve('only-on-owner.bin');
  await writeFile(path, bytes, { flag: 'wx' });
  await api.send('DOM.setFileInputFiles', { nodeId, files: [path] });
  const uploaded = await page.locator('#upload').evaluate(async element => Array.from(new Uint8Array(await element.files[0].arrayBuffer())));
  assert.deepEqual(Buffer.from(uploaded), bytes);
  await assert.rejects(api.send('DOM.setFileInputFiles', { nodeId, files: ['../.isolated-app'] }), /file_denied/);
  const browserScope = await client.newBrowserCDPSession();
  await browserScope.send('Browser.setDownloadBehavior', { behavior: 'allow', downloadPath: process.cwd(), eventsEnabled: true });
  const finished = once(browserScope, 'Browser.downloadProgress', { signal: AbortSignal.timeout(15_000) });
  const [event] = await Promise.all([finished, page.getByRole('link', { name: 'Download' }).click()]).then(([progress]) => progress);
  assert.equal(event.state, 'completed');
  assert.equal(event.filePath, resolve('remote.bin'));
  const downloaded = await readFile(event.filePath);
  assert.deepEqual(downloaded, Buffer.from(Array.from({ length: 2560 }, (_, i) => i % 256)));
  report={ uploadBytes: bytes.length, downloadBytes: downloaded.length, ownerDownload: event.filePath };
} finally {
  lines.close();
  if (gateway.exitCode === null && gateway.signalCode === null) gateway.kill('SIGTERM');
  await exited;
  await client?.close().catch(() => {});
}
const tool=resolve('../agent-browser-linux-musl-x64');
if (existsSync(tool)) {
  const {observeAgentBrowser}=await import('./agent-browser.mjs');
  await observeAgentBrowser({tool,binary:process.env.LUVUS_APP_BINARY,browserId:process.argv[2],env:process.env,workspace:process.cwd(),home:process.env.HOME});
  report.ownerAgentBrowser=true;
}
console.log(JSON.stringify(report));
