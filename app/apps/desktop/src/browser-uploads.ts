import { mkdtemp, open, rmdir, unlink, type FileHandle } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fail, fields, integer, MAX_CDP_BYTES, record, text } from './browser-policy.js';
import type { BrowserFileBudget } from './browser-file-budget.js';

interface Staged {
  name: string;
  size: number;
  offset: number;
  directory?: string;
  path?: string;
  file?: FileHandle;
  complete: boolean;
  reservation: ReturnType<BrowserFileBudget['reserve']>;
}

/** Owned by exactly one debugger lease. Only locally generated paths reach Chromium. */
export class BrowserUploads {
  private readonly staged = new Map<string, Staged>();
  private closed = false;
  private pending?: Promise<void>;
  private disposal?: Promise<void>;
  constructor(private readonly budget: BrowserFileBudget) {}

  append(value: unknown, check: () => void): Promise<void> {
    if (this.closed || this.pending) fail('control_denied');
    const job = this.write(value, check).finally(() => { this.pending = undefined; });
    this.pending = job;
    return job;
  }

  private async write(value: unknown, check: () => void): Promise<void> {
    check();
    const data = record(value);
    fields(data, ['handle', 'name', 'size', 'offset', 'data']);
    const handle = text(data.handle, 32);
    const name = text(data.name, 255);
    if (!/^[0-9a-f]{32}$/.test(handle) || name === '.' || name === '..' || /[\\/:\x00-\x1f\x7f]/.test(name)) fail('invalid_params');
    const size = integer(data.size, 0, MAX_CDP_BYTES);
    const offset = integer(data.offset, 0, size);
    const encoded = text(data.data, 8192, true);
    const bytes = Buffer.from(encoded, 'base64');
    if (bytes.toString('base64') !== encoded || offset + bytes.length > size || (!bytes.length && size !== 0)) fail('invalid_params');
    let entry = this.staged.get(handle);
    if (!entry) {
      if (offset !== 0) fail('invalid_params');
      if (this.staged.size >= 128) fail('limit_exceeded');
      entry = { name, size, offset: 0, complete: false, reservation: this.budget.reserve(size) };
      this.staged.set(handle, entry);
      entry.directory = await mkdtemp(join(tmpdir(), 'luvus-browser-upload-'));
      check();
      const path = join(entry.directory, name);
      entry.file = await open(path, 'wx', 0o600);
      entry.path = path; // Cleanup owns the name only after exclusive creation.
    }
    check();
    if (entry.size !== size || entry.name !== name || entry.offset !== offset || entry.complete || !entry.file) fail('invalid_params');
    await entry.file.writeFile(bytes);
    check();
    entry.offset += bytes.length;
    if (entry.offset === size) {
      await entry.file.close();
      entry.file = undefined;
      check();
      entry.complete = true;
      entry.reservation.complete();
    }
  }

  paths(handles: unknown): string[] {
    if (this.closed || !Array.isArray(handles) || handles.length > 16) fail('invalid_params');
    return handles.map(handle => {
      const entry = this.staged.get(text(handle, 32));
      if (!entry?.complete || !entry.path) fail('invalid_params');
      return entry.path;
    });
  }

  dispose(): Promise<void> {
    this.closed = true;
    return this.disposal ??= this.clean();
  }

  private async clean(): Promise<void> {
    await this.pending?.catch(() => {});
    for (const entry of this.staged.values()) {
      await entry.file?.close().catch(() => {});
      if (entry.path) await unlink(entry.path).catch(() => {});
      if (entry.directory) await rmdir(entry.directory).catch(() => {});
      entry.reservation.release();
    }
    this.staged.clear();
  }
}
