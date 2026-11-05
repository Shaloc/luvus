import test from 'node:test';
import assert from 'node:assert/strict';
import { accessibleText, cellAt, cellPoint, keyRoute, selectionRange, styledRuns, takeInputBatch, terminalButton } from './model.ts';

test('key phases stay ordered and bounded on either side of a geometry or focus barrier', () => {
  const keys = Array.from({length: 12}, (_, id) => ({ type: 'input', id, input: { kind: 'key', release: id % 2 === 1 } }));
  for (const barrier of [{ type: 'viewport' }, { type: 'focus' }, { type: 'input', input: { kind: 'mouse' } }]) {
    const queue = [...keys, barrier, ...keys.slice(0, 2)];
    assert.deepEqual(takeInputBatch(queue), keys.slice(0, 8));
    assert.deepEqual(takeInputBatch(queue), keys.slice(8));
    assert.deepEqual(takeInputBatch(queue), [barrier]);
    assert.deepEqual(takeInputBatch(queue), keys.slice(0, 2));
    assert.deepEqual(queue, []);
  }
});

const cell = (row, col, text, width = 1, mods = 0) => ({ row, col, text, width, mods, fg: 0, bg: 0, link: null });
const frame = {
  cols: 8, rows: 3, wraps: [true, false, false],
  cells: [cell(0, 0, ' '), cell(0, 1, ' '), cell(0, 2, '界', 2), cell(0, 3, '', 0), cell(0, 4, 'e\u0301'), cell(0, 5, 'a'), cell(0, 6, 'b'), cell(0, 7, 'c'), cell(1, 0, 'X'), cell(2, 0, '👩\u200d💻', 2), cell(2, 1, '', 0)],
};

test('styled runs use engine advances, retain combining/emoji and join shaping context', () => {
  const runs = styledRuns(frame.cells);
  assert.deepEqual(runs.map(({ row, col, text, width }) => ({ row, col, text, width })), [
    { row: 0, col: 0, text: '  ', width: 2 },
    { row: 0, col: 2, text: '界', width: 2 },
    { row: 0, col: 4, text: 'e\u0301abc', width: 4 },
    { row: 1, col: 0, text: 'X', width: 1 },
    { row: 2, col: 0, text: '👩\u200d💻', width: 2 },
  ]);
  assert.equal(styledRuns([cell(0, 0, 'ل'), cell(0, 1, 'ا')])[0].text, 'لا');
  assert.equal(styledRuns([cell(0, 0, 'a'), cell(0, 1, 'b', 1, 1)]).length, 2);
});

test('accessible rows preserve indentation, join soft wraps and keep hard breaks', () => {
  assert.equal(accessibleText(frame), '  界e\u0301abcX\n👩\u200d💻');
});

test('selection normalizes reverse ranges, wide continuations, words and lines', () => {
  assert.equal(cellAt(frame, [0, 3]).text, '界');
  assert.deepEqual(selectionRange(frame, [0, 3], [0, 3], 'cell'), [[0, 2], [0, 3]]);
  assert.deepEqual(selectionRange(frame, [1, 0], [0, 3], 'cell'), [[0, 2], [1, 0]]);
  assert.deepEqual(selectionRange(frame, [0, 4], [0, 4], 'word'), [[0, 2], [0, 7]]);
  assert.deepEqual(selectionRange(frame, [1, 2], [0, 3], 'line'), [[0, 0], [1, 7]]);
  assert.deepEqual(cellPoint(-8, 100, 9, 18, 8, 3), [2, 0]);
  assert.deepEqual([0, 1, 2, -1].map(terminalButton), [0, 1, 2, 3]);
});

const key = (overrides = {}) => ({ key: 'a', code: 'KeyA', ctrlKey: false, altKey: false, shiftKey: false, metaKey: false, repeat: false, isComposing: false, getModifierState: () => false, ...overrides });
test('key normalization leaves IME alone and routes AltGr text without Ctrl+Alt ANSI', () => {
  assert.equal(keyRoute(key()), 'text');
  assert.equal(keyRoute(key({ key: '@', ctrlKey: true, altKey: true, getModifierState: name => name === 'AltGraph' })), 'text');
  assert.equal(keyRoute(key({ key: 'é', altKey: true })), 'text');
  assert.equal(keyRoute(key({ key: 'c', ctrlKey: true })), 'key');
  assert.equal(keyRoute(key({ key: 'ArrowUp', repeat: true })), 'key');
  assert.equal(keyRoute(key({ key: 'Dead' })), 'ignore');
  assert.equal(keyRoute(key({ isComposing: true })), 'ignore');
  assert.equal(keyRoute(key({ metaKey: true })), 'ignore');
});
