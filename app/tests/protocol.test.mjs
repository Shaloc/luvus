import assert from 'node:assert/strict';
import { test } from 'node:test';
import { encodeRequest, isRequest, ResponseDecoder, MAX_ENVELOPE_BYTES } from '../packages/protocol/dist/index.js';

function frame(value) {
  const body = Buffer.from(JSON.stringify(value));
  const header = Buffer.alloc(4);
  header.writeUInt32LE(body.length);
  return Buffer.concat([header, body]);
}

test('App requests require an explicit known action and reject extra fields', () => {
  assert.equal(isRequest({ type: 'detach', id: 'one' }), true);
  assert.equal(isRequest({ type: 'detach', id: 'one', shell: 'unexpected' }), false);
  assert.equal(isRequest({ type: 'action', id: 'one', generation: 'g', method: 'shell.execute', params: {} }), false);
  const encoded = encodeRequest({ type: 'snapshot', id: 'one' });
  assert.equal(new DataView(encoded.buffer).getUint32(0, true), encoded.length - 4);
});

test('App responses tolerate fragmented headers and coalesced frames', () => {
  const received = [];
  const decoder = new ResponseDecoder(message => received.push(message));
  const first = { type: 'ready', protocol: 1, transport: 21, capabilities: ['app_projection.v1'] };
  const second = { type: 'reply', id: 'one', result: { text: '界' } };
  const bytes = Buffer.concat([frame(first), frame(second)]);
  for (let index = 0; index < bytes.length; index += 3) decoder.push(bytes.subarray(index, index + 3));
  decoder.finish();
  assert.deepEqual(received, [first, second]);
});

test('App response sizes and incomplete data fail closed', () => {
  const oversized = Buffer.alloc(4);
  oversized.writeUInt32LE(MAX_ENVELOPE_BYTES + 1);
  assert.throws(() => new ResponseDecoder(() => {}).push(oversized), /length/);
  const decoder = new ResponseDecoder(() => {});
  decoder.push(Buffer.from([1, 0]));
  assert.throws(() => decoder.finish(), /Truncated/);
  assert.throws(() => new ResponseDecoder(() => {}).push(frame({ type: 'unknown' })), /Invalid/);
});

test('App chunks are ordered and produce a single validated response', () => {
  const received = [];
  const decoder = new ResponseDecoder(message => received.push(message));
  const response = { type: 'reply', id: 'one', result: '多字节' };
  const serialized = JSON.stringify(response);
  decoder.push(frame({ type: 'chunk', transfer: 't', index: 0, total: 2, text: serialized.slice(0, 12) }));
  assert.equal(received.length, 0);
  decoder.push(frame({ type: 'chunk', transfer: 't', index: 1, total: 2, text: serialized.slice(12) }));
  decoder.finish();
  assert.deepEqual(received, [response]);
  assert.throws(() => new ResponseDecoder(() => {}).push(frame({ type: 'chunk', transfer: 't', index: 1, total: 2, text: '' })), /transfer/);
});

test('App chunks cannot nest or consume unbounded parallel transfers', () => {
  const decoder = new ResponseDecoder(() => {});
  for (const transfer of ['a', 'b']) decoder.push(frame({ type: 'chunk', transfer, index: 0, total: 2, text: '' }));
  assert.throws(() => decoder.push(frame({ type: 'chunk', transfer: 'c', index: 0, total: 2, text: '' })), /transfer/);
  const nested = { type: 'chunk', transfer: 'inner', index: 0, total: 1, text: '{}' };
  assert.throws(() => new ResponseDecoder(() => {}).push(frame({ type: 'chunk', transfer: 'outer', index: 0, total: 1, text: JSON.stringify(nested) })), /assembled/);
});
