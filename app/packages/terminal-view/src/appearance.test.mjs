import assert from 'node:assert/strict';
import { test } from 'node:test';
import { CanvasTerminal } from '../dist/index.js';

test('terminal painting uses distinct continuous regular and bold weights', t => {
  const previous = globalThis.window;
  globalThis.window = { devicePixelRatio: 2 };
  t.after(() => { if (previous) globalThis.window = previous; else delete globalThis.window; });
  const fonts = [];
  const context = new Proxy({ fillText() { fonts.push(this.font); } }, {
    get(target, key) { return target[key] ?? (() => {}); },
  });
  const terminal = Object.assign(Object.create(CanvasTerminal.prototype), {
    canvas: { getContext: () => context }, width: 300, height: 30,
    host: { style: { setProperty() {} } },
    textarea: { style: {} }, preedit: { style: {} }, accessible: {}, images: { draw() {} },
    fontFamily: 'monospace', fontSize: 14, fontWeight: 450, fontBoldWeight: 760,
    cellWidth: 8, cellHeight: 20, backgroundOpacity: 1,
    background: '#fff', foreground: '#222', ansi: [],
    frame: { cols: 3, rows: 1, wraps: [false], cells: [0, 1, 5].map((mods, col) => ({
      col, row: 0, width: 1, text: 'M', mods, fg: null, bg: null,
    })) },
  });
  terminal.paint();
  assert.deepEqual(fonts, ['450 14px monospace', '760 14px monospace', 'italic 760 14px monospace']);
  let measured = 0;
  terminal.measure = () => measured++;
  terminal.paint = () => {};
  terminal.setAppearance({ fontSize: 15, fontFamily: 'monospace', fontWeight: 437, fontBoldWeight: 735,
    backgroundOpacity: 1, rightClickToApp: false, foreground: '#222', background: '#fff' });
  assert.equal(terminal.textarea.style.font, '437 15px monospace');
  assert.equal(terminal.preedit.style.font, terminal.textarea.style.font);
  assert.equal(terminal.fontBoldWeight, 735);
  assert.equal(measured, 1);
});
