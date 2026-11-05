// No --session and no pre-started new core: reproduce a Finder launch beside
// a released transport-20 owner, using only a marker-checked private home.
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { _electron } from 'playwright-core';
import { isolatedEnvironment } from '../scripts/dev.mjs';

const exec = promisify(execFile);
const repo = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const bundle = resolve(process.env.LUVUS_TEST_BUNDLE ?? '/Applications/Luvus.app');
const binary = join(bundle, 'Contents/Resources/luvus');
const legacy = process.env.LUVUS_TEST_LEGACY_BINARY;
assert.ok(legacy && legacy.startsWith('/'), 'LUVUS_TEST_LEGACY_BINARY must name the exact released transport-20 binary');
assert.match((await exec(legacy, ['--version'], { timeout: 5000 })).stdout, /transport=20\b/);
const root = await mkdtemp(join(repo, 'target/app-e2e-startup-'));
await writeFile(join(root, '.isolated-app'), root, { flag: 'wx', mode: 0o600 });
const home = join(root, 'home'), profile = join(root, 'profile'), cwd = join(root, 'project');
for (const dir of [home, profile, cwd]) await mkdir(dir, { mode: 0o700 });
const env = { ...isolatedEnvironment(), HOME: root, LUVUS_HOME: home, LUVUS_APP_USER_DATA: profile,
  XDG_CONFIG_HOME: join(home, 'config'), XDG_DATA_HOME: join(home, 'data'), XDG_CACHE_HOME: join(home, 'cache'),
  ZDOTDIR: home, SHELL: '/bin/sh', TERM: 'xterm-256color' };
for (const key of Object.keys(env)) if (/^(LUVUS_SESSION|LUVUS_SOCKET_PATH|VITE_.*|LUVUS_APP_BINARY|LUVUS_APP_DEV_URL|ELECTRON_RUN_AS_NODE)$/.test(key)) delete env[key];
await writeFile(join(home, 'config.json'), JSON.stringify({ check_updates: false, language: 'en', shell: '/bin/sh' }));
const cli = async (exe, name, ...args) => (await exec(exe, ['--session', name, ...args], { env, cwd, timeout: 15_000, maxBuffer: 4 * 1024 * 1024 })).stdout;
const snapshot = async name => JSON.parse(await cli(binary, name, 'uhp', 'snapshot')).result;
const report = { root, bundle, legacy, checks: [] };
const check = text => { report.checks.push(text); console.log('PASS ' + text); };
let desktop, page, selected;
async function launch(answer, args = []) {
  desktop = await _electron.launch({ executablePath: join(bundle, 'Contents/MacOS/Luvus'), args, env, cwd, timeout: 35_000 });
  // Only the native user's choice is substituted. The installed application
  // performs all compatibility checks and starts its own real bundled server.
  await desktop.evaluate(({ dialog }, response) => {
    globalThis.startupDialogs = [];
    dialog.showMessageBox = async (...args) => {
      globalThis.startupDialogs.push(args.at(-1));
      return { response: globalThis.startupDialogs.length === 1 ? response : 0, checkboxChecked: false };
    };
  }, answer);
  page = await desktop.firstWindow();
  page.setDefaultTimeout(10_000);
}
try {
  await cli(legacy, 'default', 'server', 'start');
  const before = await snapshot('default');
  const pidBefore = await readFile(join(home, 'server.pid'), 'utf8');
  report.legacyGeneration = before.server_generation;
  await launch(0);
  await page.getByText(/transport 20; this app requires 21/).waitFor();
  const prompts = await desktop.evaluate(() => globalThis.startupDialogs);
  assert.equal(prompts.length, 1);
  assert.deepEqual(prompts[0].buttons, ['Cancel', 'Retry', 'Open Separate Desktop Session']);
  assert.ok(!await page.locator('.connection-dot.online').count());
  assert.equal((await snapshot('default')).server_generation, before.server_generation);
  assert.equal(await readFile(join(home, 'server.pid'), 'utf8'), pidBefore);
  await page.screenshot({ path: join(root, 'legacy-detected.png') });
  check('no-argument cold launch detects the old live transport before HelloApp; cancel leaves owner intact');
  await desktop.close(); desktop = undefined;

  await launch(2);
  await page.locator('.connection-dot.online').waitFor({ timeout: 30_000 });
  selected = JSON.parse(await readFile(join(profile, 'desktop-session.json'), 'utf8')).session;
  assert.match(selected, /^desktop-[0-9a-f-]{36}$/);
  const current = await snapshot(selected);
  assert.notEqual(current.server_generation, before.server_generation);
  assert.equal((await snapshot('default')).server_generation, before.server_generation);
  assert.equal(await readFile(join(home, 'server.pid'), 'utf8'), pidBefore);
  await page.locator('canvas').first().click();
  await page.keyboard.type("printf '%s%s\\n' DESKTOP_ RECOVERED");
  await page.keyboard.press('Enter');
  await page.waitForFunction(() => document.querySelector('.terminal-accessible')?.textContent.includes('DESKTOP_RECOVERED'));
  await page.screenshot({ path: join(root, 'recovered.png') });
  check('explicit separate-session recovery renders a working terminal and preserves the old PID and generation');
  report.desktopSession = selected;
  report.desktopGeneration = current.server_generation;
  await desktop.close(); desktop = undefined;

  await launch(0);
  await page.locator('.connection-dot.online').waitFor({ timeout: 30_000 });
  assert.equal((await snapshot(selected)).server_generation, current.server_generation);
  assert.deepEqual(await desktop.evaluate(() => globalThis.startupDialogs), []);
  assert.equal(await readFile(join(home, 'server.pid'), 'utf8'), pidBefore);
  check('no-argument relaunch remembers the selected session without asking again or restarting either owner');
  await desktop.close(); desktop = undefined;
  await launch(0, ['--session', 'default']);
  await page.getByText(/transport 20; this app requires 21/).waitFor();
  assert.equal(JSON.parse(await readFile(join(profile, 'desktop-session.json'), 'utf8')).session, selected);
  assert.equal(await readFile(join(home, 'server.pid'), 'utf8'), pidBefore);
  check('an explicit session overrides the saved choice and is never silently redirected');
} finally {
  try {
    if (page && !page.isClosed()) await page.screenshot({ path: join(root, 'last-screen.png') }).catch(() => {});
    await desktop?.close();
  } finally {
    assert.equal(await readFile(join(root, '.isolated-app'), 'utf8'), root);
    if (!selected) selected = await readFile(join(profile, 'desktop-session.json'), 'utf8').then(s => JSON.parse(s).session).catch(() => undefined);
    if (selected && /^desktop-[0-9a-f-]{36}$/.test(selected)) await cli(binary, selected, 'server', 'stop');
    await cli(legacy, 'default', 'server', 'stop');
    await writeFile(join(root, 'result.json'), JSON.stringify(report, null, 2));
    console.log('Startup fixture: ' + root);
  }
}
