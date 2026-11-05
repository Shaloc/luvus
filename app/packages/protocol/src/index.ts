import Ajv from 'ajv';
import schema from '../generated/schema.json' with { type: 'json' };
import type { Request, Response } from '../generated/contracts.js';

export type { Request, Response, Target, Input, Cell, Cursor, TerminalFrame } from '../generated/contracts.js';
export const MAX_ENVELOPE_BYTES = 1024 * 1024;
export const MAX_OBJECT_BYTES = 64 * 1024 * 1024;
const encoder = new TextEncoder();
const decoder = new TextDecoder('utf-8', { fatal: true });
const ajv = new Ajv({ strict: false, allErrors: false });
const validateRequest = ajv.compile<Request>({ ...schema, oneOf: undefined, $ref: '#/$defs/Request' });
const validateResponse = ajv.compile<Response>({ ...schema, oneOf: undefined, $ref: '#/$defs/Response' });

export function isRequest(value: unknown): value is Request { return validateRequest(value); }
export function isResponse(value: unknown): value is Response { return validateResponse(value); }

export function encodeRequest(request: Request): Uint8Array {
  if (!isRequest(request)) throw new Error('Invalid App request');
  const bytes = encoder.encode(JSON.stringify(request));
  if (bytes.length > MAX_ENVELOPE_BYTES) throw new Error('App request exceeds the envelope limit');
  const frame = new Uint8Array(bytes.length + 4);
  new DataView(frame.buffer).setUint32(0, bytes.length, true);
  frame.set(bytes, 4);
  return frame;
}

export class ResponseDecoder {
  private buffered = new Uint8Array();
  private readonly transfers = new Map<string, { next: number; total: number; chunks: string[]; bytes: number }>();
  private transferBytes = 0;

  constructor(private readonly deliver: (response: Response) => void) {}

  push(chunk: Uint8Array): void {
    let offset = 0;
    while (offset < chunk.length) {
      const needed = this.buffered.length < 4 ? 4 : 4 + this.length();
      const count = Math.min(needed - this.buffered.length, chunk.length - offset);
      const next = new Uint8Array(this.buffered.length + count);
      next.set(this.buffered);
      next.set(chunk.subarray(offset, offset + count), this.buffered.length);
      this.buffered = next;
      offset += count;
      if (this.buffered.length < 4) continue;
      const length = this.length();
      if (this.buffered.length !== length + 4) continue;
      const response: unknown = JSON.parse(decoder.decode(this.buffered.subarray(4)));
      this.buffered = new Uint8Array();
      this.accept(response);
    }
  }

  finish(): void {
    if (this.buffered.length || this.transfers.size) throw new Error('Truncated App response');
  }

  private length(): number {
    const length = new DataView(this.buffered.buffer, this.buffered.byteOffset, 4).getUint32(0, true);
    if (length === 0 || length > MAX_ENVELOPE_BYTES) throw new Error('Invalid App envelope length');
    return length;
  }

  private accept(value: unknown): void {
    if (!isResponse(value)) throw new Error('Invalid App response');
    if (value.type !== 'chunk') { this.deliver(value); return; }
    let transfer = this.transfers.get(value.transfer);
    if (!transfer) {
      if (value.index !== 0 || this.transfers.size >= 2) throw new Error('Invalid App transfer');
      transfer = { next: 0, total: value.total, chunks: [], bytes: 0 };
      this.transfers.set(value.transfer, transfer);
    }
    if (value.total !== transfer.total || value.index !== transfer.next) throw new Error('Out-of-order App transfer');
    const bytes = encoder.encode(value.text).byteLength;
    this.transferBytes += bytes;
    transfer.bytes += bytes;
    if (this.transferBytes > MAX_OBJECT_BYTES) throw new Error('App transfer exceeds the attachment budget');
    transfer.chunks.push(value.text);
    transfer.next += 1;
    if (transfer.next !== transfer.total) return;
    this.transfers.delete(value.transfer);
    this.transferBytes -= transfer.bytes;
    const response: unknown = JSON.parse(transfer.chunks.join(''));
    if (!isResponse(response) || response.type === 'chunk') throw new Error('Invalid assembled App response');
    this.deliver(response);
  }
}
