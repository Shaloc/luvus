import assert from 'node:assert/strict';
import { test } from 'node:test';
import { CanvasTerminal } from '../dist/index.js';

const target = { generation: 'server', workspace_id: 'workspace', tab_id: 'tab', pane_id: 'pane', terminal_id: 'terminal' };
const key = { kind: 'key', key: 'b', ctrl: true, alt: false, shift: false, repeat: false, release: false };
const frame = (route = target) => ({ target: route, cols: 2, rows: 2, cells: [], revision: '1', geometry: '1' });
function fixture() {
  const requests = [], errors = [], removed = { remove() {} };
  const terminal = Object.assign(Object.create(CanvasTerminal.prototype), {
    options: { target, send: async request => { requests.push(request); }, onError: error => errors.push(error) },
    frame: null, previewFrame: null, awaitingFrame: [], awaitingFrameBytes: 0,
    queue: [], sending: false, held: new Map(), disposed: false,
    images: { update() {}, dispose() {} }, abort: new AbortController(), observer: { disconnect() {} },
    canvas: removed, textarea: removed, accessible: removed, waiting: removed, preedit: removed,
    paint() {}, schedule() {},
  });
  return { terminal, requests, errors };
}
const drained = () => new Promise(resolve => setImmediate(resolve));

test('input before the live frame waits for its exact terminal and keeps press/release order', async () => {
  const { terminal, requests, errors } = fixture();
  terminal.input(key);
  terminal.input({ ...key, release: true });
  terminal.input({ kind: 'paste', text: 'hello' });
  terminal.input({ kind: 'focus_event', focused: true });
  terminal.preview(frame());
  terminal.update(frame({ ...target, terminal_id: 'replacement' }));
  await drained();
  assert.equal(requests.length, 0);
  assert.equal(terminal.awaitingFrame.length, 3);
  terminal.update(frame());
  await drained();
  assert.deepEqual(requests.map(request => request.input), [key, { ...key, release: true }, { kind: 'paste', text: 'hello' }]);
  assert.ok(requests.every(request => request.target === target));
  assert.equal(terminal.awaitingFrame.length, 0);
  assert.equal(terminal.awaitingFrameBytes, 0);
  assert.equal(errors.length, 0);
});

test('waiting input is bounded by event count and UTF-8 bytes', () => {
  const byCount = fixture();
  for (let i = 0; i < 257; i++) byCount.terminal.input(key);
  assert.equal(byCount.terminal.awaitingFrame.length, 256);
  assert.equal(byCount.errors.length, 1);
  const byBytes = fixture();
  byBytes.terminal.input({ kind: 'paste', text: '\u4e2d'.repeat(90_000) });
  assert.equal(byBytes.terminal.awaitingFrame.length, 0);
  assert.equal(byBytes.errors.length, 1);
});

test('disposing an unfed terminal drops pending keys even if a late frame arrives', async t => {
  const cancel = globalThis.cancelAnimationFrame;
  globalThis.cancelAnimationFrame = () => {};
  t.after(() => { if (cancel) globalThis.cancelAnimationFrame = cancel; else delete globalThis.cancelAnimationFrame; });
  const { terminal, requests } = fixture();
  terminal.input(key);
  terminal.held.set('KeyB', key);
  terminal.dispose();
  terminal.input({ kind: 'paste', text: 'stale' });
  terminal.update(frame());
  await drained();
  assert.equal(terminal.awaitingFrame.length, 0);
  assert.equal(terminal.held.size, 0);
  assert.equal(requests.length, 0);
  assert.equal(terminal.frame, null);
});
