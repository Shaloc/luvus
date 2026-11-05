import { randomBytes } from 'node:crypto';
import { constants } from 'node:fs';
import { mkdtemp, open, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { allowedUrl, fail, MAX_CDP_BYTES } from './browser-policy.js';
import type { BrowserFileBudget } from './browser-file-budget.js';

interface Route { permit: string; directory: string; session?: string }

/** Native downloads default to deny. A permit represents one owner-chosen
 * directory, but its actual path is never sent to the display. */
export class BrowserDownloads {
  private route?: Route;
  private active?: { item: Electron.DownloadItem; task: Promise<void>; cancel: () => void };
  private serial = 0;
  private closed = false;
  private jobs = new Set<Promise<void>>();
  private count = 0;

  constructor(private readonly check: () => void, private readonly send: (raw: string) => Promise<void>,
    private readonly failed: () => void, private readonly budget: BrowserFileBudget) {}

  async configure(permit: string | null, session?: string): Promise<void> {
    this.check();
    const serial = ++this.serial;
    await this.reset();
    this.check();
    if (serial !== this.serial) fail('control_revoked');
    if (permit === null) return;
    const job = (async () => {
      const directory = await mkdtemp(join(tmpdir(), 'luvus-browser-download-'));
      try {
        this.check();
        if (this.closed || this.serial !== serial) fail('control_revoked');
        this.route = { permit, directory, session };
      } catch (error) { await rm(directory, { recursive: true, force: true }); throw error; }
    })();
    this.jobs.add(job);
    try { await job; } finally { this.jobs.delete(job); }
  }

  accept(item: Electron.DownloadItem, frame: string): boolean {
    const route = this.route;
    if (!route || this.active || this.closed || this.count >= 128) return false;
    try { this.check(); } catch { return false; }
    const size = item.getTotalBytes();
    const url = item.getURL(), name = item.getFilename();
    // A missing Content-Length is unbounded. Never start a native disk download
    // in that case, and never use the server-supplied filename on the display.
    if (!Number.isSafeInteger(size) || size <= 0 || size > MAX_CDP_BYTES || !allowedUrl(url)
        || !/^https?:\/\//.test(url) || !name || Buffer.byteLength(name) > 255
        || name === '.' || name === '..' || /[\\/:\x00-\x1f\x7f]/.test(name)) return false;
    this.count++;
    const guid = randomBytes(16).toString('hex');
    const path = join(route.directory, guid);
    let reservation: ReturnType<BrowserFileBudget['reserve']> | undefined;
    try { reservation = this.budget.reserve(size); item.setSavePath(path); }
    catch { reservation?.release(); return false; }
    let finished = false;
    let cancelWait!: () => void;
    const cancelled=new AbortController();
    const done = new Promise<string>(resolve => {
      let deadline:ReturnType<typeof setTimeout>|undefined;
      const finish=(state:string)=>{finished=true;clearTimeout(deadline);resolve(state);};
      item.once('done', (_event, state) => finish(state));
      cancelWait = () => {
        if(cancelled.signal.aborted)return;
        cancelled.abort();
        if(!finished){
          // cancel() requests cancellation. Chromium can still create its
          // intermediate save directory until it acknowledges with 'done'.
          deadline=setTimeout(()=>finish('cancelled'),2000);
          try{item.cancel();}catch{finish('cancelled');}
        }
      };
    });
    item.on('updated', () => {
      if (item.getReceivedBytes() > size || item.getTotalBytes() !== size || this.route !== route) item.cancel();
    });
    const write = async(data: Record<string, unknown>) => {
      this.check();
      if (this.route !== route || cancelled.signal.aborted) fail('control_revoked');
      // An owner acknowledgement can still be in flight when a human takes
      // over. Local staging cleanup must not wait for that old request.
      let stop!:()=>void;
      const revocation=new Promise<never>((_,reject)=>{stop=()=>reject(Error('control_revoked'));});
      cancelled.signal.addEventListener('abort',stop,{once:true});
      try{await Promise.race([this.send(JSON.stringify({download:{permit:route.permit,guid,body:data}})),revocation]);}
      finally{cancelled.signal.removeEventListener('abort',stop);}
    };
    const task = (async () => {
      try {
        await write({ kind: 'begin', name, frame, url, size, session: route.session });
        if (await done !== 'completed') { await write({ kind: 'cancel' }); return; }
        const file = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW);
        try {
          const stat = await file.stat();
          if (!stat.isFile() || stat.size !== size) fail('limit_exceeded');
          const buffer = Buffer.alloc(64 * 1024);
          for (let offset = 0; offset < size;) {
            const { bytesRead } = await file.read(buffer, 0, Math.min(buffer.length, size - offset), offset);
            if (!bytesRead) fail('invalid_params');
            await write({ kind: 'data', offset, data: buffer.subarray(0, bytesRead).toString('base64') });
            offset += bytesRead;
          }
          await write({ kind: 'end' });
        } finally { await file.close(); }
      } catch {
        cancelWait();
        if (this.route === route && !this.closed) this.failed();
      } finally {
        if(cancelled.signal.aborted)await done;
        await rm(path, { force: true }).catch(() => {});
        reservation.release();
        if (this.active?.item === item) this.active = undefined;
      }
    })();
    this.active = { item, task, cancel: cancelWait };
    return true;
  }

  private async reset(): Promise<void> {
    const route = this.route;
    this.route = undefined;
    const active = this.active;
    if (active) {
      active.cancel();
      await active.task;
    }
    if (route) await rm(route.directory, { recursive: true, force: true, maxRetries:3,retryDelay:50 });
  }

  async dispose(): Promise<void> {
    this.closed = true;
    ++this.serial;
    await this.reset();
    await Promise.allSettled(this.jobs);
  }
}
