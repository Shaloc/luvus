import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { mkdtemp, mkdir, readFile, realpath, writeFile } from 'node:fs/promises';
import { dirname, resolve, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { _electron } from 'playwright-core';

const execute = promisify(execFile);
const root = await realpath(resolve(dirname(fileURLToPath(import.meta.url)), '../..'));
const index = process.argv.indexOf('--luvus');
const binary = await realpath(resolve(index >= 0 ? process.argv[index + 1] : join(root, 'target/debug/luvus')));
assert.ok(binary.startsWith(join(root, 'target') + '/'), 'E2E requires a binary built in this checkout');
const target = join(root, 'target');
const home = await mkdtemp(join(target, 'app-e2e-'));
const workspace = join(home, 'workspace');
await mkdir(workspace);
await mkdir(join(home, 'electron'));
await writeFile(join(home, '.isolated-app'), home);
await writeFile(join(workspace, 'example.txt'), 'Native file view\n多字节文字\n');
await writeFile(join(home, 'config.json'), JSON.stringify({ check_updates: false, language: 'en', theme: 'quattro-rally' }));
const env = Object.fromEntries(Object.entries(process.env).filter(([name]) => !name.startsWith('LUVUS_') && name !== 'ELECTRON_RUN_AS_NODE'));
Object.assign(env, { HOME: home, LUVUS_HOME: home, LUVUS_SESSION: 'app-e2e', LUVUS_APP_BINARY: binary, LUVUS_APP_USER_DATA: join(home, 'electron'), SHELL: '/bin/sh', TERM: 'xterm-256color' });
const command = (...args) => execute(binary, ['--session', 'app-e2e', ...args], { env, cwd: workspace, timeout: 15000, maxBuffer: 1024 * 1024 });
let desktop;
let started = false;
try {
  await command('server', 'start');
  started = true;
  await command('workspace', 'open', workspace);
  desktop = await _electron.launch({ args: [join(root, 'app/apps/desktop/dist/main.cjs')], env, cwd: workspace, timeout: 45000 });
  const page = await desktop.firstWindow();
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.locator('canvas').first().waitFor({ state: 'visible', timeout: 30000 });
  await page.locator('canvas').first().click();
  await page.keyboard.type("printf 'luvus-\\141pp-input-ok\\n'");
  await page.keyboard.press('Enter');
  await page.waitForFunction(async () => {
    const messages = await window.luvus.bootstrap();
    return messages.some(message => message.type === 'terminal' && message.frame.cells.map(cell => cell.text).join('').includes('luvus-app-input-ok'));
  }, null, { timeout: 20000 });
  const state = await page.evaluate(() => window.luvus.bootstrap());
  const snapshot = state.find(message => message.type === 'snapshot')?.snapshot;
  assert.ok(snapshot?.server_generation);
  assert.equal(typeof snapshot.event_sequence, 'string');
  await command('files', 'open', join(workspace, 'example.txt'), '--target', 'pane');
  await page.getByText('Native file view', { exact: false }).first().waitFor({ timeout: 20000 });
  await page.screenshot({ path: join(home, 'desktop.png') });
  await page.setViewportSize({ width: 900, height: 600 });
  await page.screenshot({ path: join(home, 'desktop-compact.png') });
  assert.deepEqual(errors, [], 'renderer emitted uncaught errors');
  await desktop.close();
  desktop = undefined;
  await command('server', 'status');
  const after = JSON.parse((await command('uhp', 'snapshot')).stdout);
  const model = after.result ?? after;
  assert.equal(model.server_generation, snapshot.server_generation, 'closing App restarted the owner');
  console.log(`PASS: real Electron terminal input, native file view, compact layout and detach preservation; artifacts ${home}`);
} finally {
  await desktop?.close();
  if (started) {
    assert.equal(await readFile(join(home, '.isolated-app'), 'utf8'), home);
    await command('server', 'stop').catch(error => console.error(`Fixture stop failed: ${error.message}`));
  }
}
