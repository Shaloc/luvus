import assert from 'node:assert/strict';
import test from 'node:test';
import { terminalAnsiPalette, unpackColor } from '../dist/colors.js';

test('named and bright ANSI slots follow the selected palette without rewriting RGB or the xterm cube', () => {
  const before = terminalAnsiPalette(false);
  const after = terminalAnsiPalette(false, {blue:0x0281a1c1});
  for (const index of [4, 12]) {
    assert.equal(unpackColor(0x01000000 + index, '', before), '#89b4fa');
    assert.equal(unpackColor(0x01000000 + index, '', after), '#81a1c1');
  }
  assert.equal(unpackColor(0x02000080, '', after), '#000080');
  assert.equal(unpackColor(0x01000015, '', after), 'rgb(0 0 255)');
  assert.equal(unpackColor(0x010000ff, '', after), 'rgb(238 238 238)');
  assert.equal(unpackColor(0, '#123456', after), '#123456');
});

test('light palettes remain distinct and invalid or indexed theme roles cannot replace ANSI slots', () => {
  assert.equal(terminalAnsiPalette(true)[4], '#1e66f5');
  assert.notEqual(terminalAnsiPalette(true)[4], terminalAnsiPalette(false)[4]);
  assert.deepEqual(terminalAnsiPalette(false, {blue:0x01000004, green:'invalid', cyan:NaN}), terminalAnsiPalette(false));
});
