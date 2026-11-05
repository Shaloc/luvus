import assert from 'node:assert/strict';
import { test } from 'node:test';
import { execFile, spawn } from 'node:child_process';
import { mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { encodeRequest, ResponseDecoder } from '../packages/protocol/dist/index.js';

const execute = promisify(execFile);
const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const binary = join(root, 'target/debug/luvus');

test('App attachment controls a real core without a TUI and preserves the owner on detach', { timeout: 45000 }, async () => {
  const home = await mkdtemp(join(root, 'target/app-e2e-protocol-'));
  const workspace = join(home, 'project');
  await mkdir(workspace);
  await writeFile(join(home, '.isolated-app'), home);
  await writeFile(join(home, 'config.json'), JSON.stringify({ check_updates: false, language: 'en' }));
  const env = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('LUVUS_')));
  Object.assign(env, { HOME: home, LUVUS_HOME: home, SHELL: '/bin/sh', TERM: 'xterm-256color' });
  const command = (...args) => execute(binary, ['--session', 'app-test', ...args], { cwd: workspace, env, timeout: 10000, maxBuffer: 4 * 1024 * 1024 });
  let child;
  let stopped = false;
  let error;
  let snapshot;
  const frames = new Map();
  const replies = new Map();
  const waiters = new Set();
  const wait = predicate => new Promise((resolve, reject) => {
    const check = () => {
      if (error) { finish(); reject(error); return; }
      const value = predicate();
      if (value) { finish(); resolve(value); }
    };
    const finish = () => { clearTimeout(timer); waiters.delete(check); };
    const timer = setTimeout(() => { finish(); reject(new Error('App protocol fixture deadline elapsed')); }, 10000);
    waiters.add(check);
    check();
  });
  const request = async fields => {
    const id = crypto.randomUUID();
    child.stdin.write(encodeRequest({ ...fields, id }));
    const response = await wait(() => replies.get(id));
    replies.delete(id);
    return response;
  };
  try {
    await command('server', 'start');
    await command('workspace', 'open', workspace);
    child = spawn(binary, ['--session', 'app-test', 'client', '--surface', 'app', '--stdio'], { cwd: workspace, env, stdio: 'pipe' });
    const decoder = new ResponseDecoder(message => {
      if (message.type === 'snapshot') snapshot = message.snapshot;
      if (message.type === 'terminal') frames.set(message.frame.target.pane_id, message.frame);
      if (message.type === 'reply' || message.type === 'error' && message.id) replies.set(message.id, message);
      if (message.type === 'error' && !message.id) error = new Error(message.message);
      for (const check of [...waiters]) check();
    });
    child.stdout.on('data', bytes => { try { decoder.push(bytes); } catch (cause) { error = cause; for (const check of [...waiters]) check(); } });
    child.stderr.on('data', bytes => { error = new Error(bytes.toString()); for (const check of [...waiters]) check(); });
    const frame = await wait(() => snapshot && [...frames.values()][0]);
    const generation = snapshot.server_generation;
    assert.equal(typeof snapshot.event_sequence, 'string');
    const bad = await request({ type: 'focus', target: { ...frame.target, generation: 'stale' } });
    assert.equal(bad.type, 'error');
    assert.equal(bad.code, 'stale_target');
    const resized = await request({ type: 'viewport', target: frame.target, cols: 100, rows: 30, cell_width: 8, cell_height: 16 });
    assert.equal(resized.type, 'reply');
    await wait(() => frames.get(frame.target.pane_id)?.cols === 100);
    const pasted = await request({ type: 'input', target: frame.target, input: { kind: 'paste', text: "printf 'luvus-\\141pp-verified\\n'" } });
    assert.equal(pasted.type, 'reply');
    await request({ type: 'input', target: frame.target, input: { kind: 'key', key: 'Enter' } });
    await wait(() => frames.get(frame.target.pane_id)?.cells.map(cell => cell.text).join('').includes('luvus-app-verified'));
    child.stdin.end();
    await new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('App client did not detach')), 5000);
      child.once('exit', code => { clearTimeout(timer); code === 0 ? resolve() : reject(new Error(`App exit ${code}`)); });
    });
    stopped = true;
    const result = JSON.parse((await command('uhp', 'snapshot')).stdout);
    assert.equal((result.result ?? result).server_generation, generation);
  } finally {
    if (child && !stopped) child.kill('SIGTERM');
    assert.equal(await readFile(join(home, '.isolated-app'), 'utf8'), home);
    await command('server', 'stop');
  }
});
