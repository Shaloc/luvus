import type { BrowserWindow, Rectangle } from 'electron';
import { isRequest, type Response, type Target } from '@luvus/app-protocol';
import { BrowserHost, type BrowserCommand, type BrowserDescriptor } from './browser-host.js';
import type { CoreClient } from './core-client.js';
import { displayBrowserUrl, MAX_CDP_BYTES, MAX_CDP_EVENT_CHUNK_BYTES, MAX_CDP_MESSAGES, sameTarget } from './browser-policy.js';

interface Status {
  browser_id: string;
  url: string;
  state: 'preparing' | 'ready' | 'closed' | 'failed';
  controlled: boolean;
  title?: string;
  target?: Target;
}

export class BrowserBridge {
  private readonly host: BrowserHost;
  private readonly states = new Map<string, Status>();
  private readonly targets = new Map<string, Target>();
  private visibleTargets = new Set<string>();
  private closed = false;
  private cdpBytes = 0;
  private cdpMessages = 0;

  constructor(window: BrowserWindow, private readonly core: CoreClient, private readonly publish: (message: Response) => void) {
    this.host = new BrowserHost(window, (id, reason, generation) => {
      if (this.closed) return;
      this.update(id, { controlled: false });
      if (reason === 'browser_closed' || reason === 'render_process_gone') {
        void this.core.request({ type: 'browser_closed', id: crypto.randomUUID(), browser_id: id }).catch(() => {});
      } else {
        void this.core.request({ type: 'browser_takeover', id: crypto.randomUUID(), browser_id: id, lease_generation:generation }).catch(() => {});
      }
    }, (id, generation, raw) => this.cdpMessage(id, generation, raw), id => this.update(id, {}));
  }

  receive(message: Response): boolean {
    // Native CDP traffic is never renderer state, including unexpected internal effect variants.
    if (message.type === 'effect' && (message.kind.startsWith('browser_cdp') || message.kind === 'browser_result')) return true;
    if (message.type === 'snapshot' && !this.closed) {
      const model = object(message.snapshot);
      const workspace = values(model.workspaces).map(object).find(value => value.active);
      const tab = values(workspace?.tabs).map(object).find(value => value.active);
      this.visibleTargets = new Set(values(tab?.panes).map(object).flatMap(pane => {
        const target = this.target(pane.target);
        return target && (!tab?.zoomed || pane.pane_id === tab.focus) ? [key(target)] : [];
      }));
      for (const [id, target] of this.targets) if (!this.visibleTargets.has(key(target))) this.host.bounds(id, null);
      return false;
    }
    if (message.type !== 'effect' || !['browser_open', 'browser_command', 'browser_error', 'browser_closed'].includes(message.kind)) return false;
    if (this.closed) return true;
    const data = object(message.data);
    const id = string(data.browser_id);
    if (!/^[0-9a-f]{32}$/.test(id)) return true;
    if (message.kind === 'browser_closed') {
      this.update(id, { state: 'closed', controlled: false });
      void this.host.close(id).finally(() => this.core.request({ type: 'browser_closed', id: crypto.randomUUID(), browser_id: id }).catch(() => {}));
      return true;
    }
    const target = this.target(data.target);
    const requestId = string(data.request_id);
    const generation = string(data.lease_generation);
    if (!target || !requestId || requestId.length > 128 || !/^[0-9]{1,20}$/.test(generation)) return true;
    if (message.kind === 'browser_error') {
      this.update(id, { state: 'failed', controlled: false });
      void this.complete(requestId, target, generation, null, 'display_failed');
      return true;
    }
    if (message.kind === 'browser_open') {
      const previousTarget = this.targets.get(id);
      if (previousTarget && !sameTarget(previousTarget, target)) {
        void this.complete(requestId, target, generation, null, 'route_changed');
        return true;
      }
      const descriptor: BrowserDescriptor = {
        id, url: displayBrowserUrl(string(data.url)), proxy_port: Number(data.proxy_port), target,
        generation: target.generation, workspace_id: target.workspace_id, pane_id: target.pane_id,
        request_id: requestId, lease_generation: generation,
      };
      this.targets.set(id, target);
      this.states.set(id, { browser_id: id, url: descriptor.url, state: 'preparing', controlled: false });
      this.update(id, {});
      void this.host.open(descriptor).then(async () => {
        if (this.closed) return;
        if (await this.complete(requestId, target, generation, { ready: true }, null)) this.update(id, { state: 'ready' });
        else { await this.host.close(id); this.update(id, { state: 'closed' }); }
      }, async () => {
        this.update(id, { state: 'failed' });
        await this.complete(requestId, target, generation, null, 'display_failed');
      });
      return true;
    }
    const command: BrowserCommand = {
      request_id: requestId, browser_id: id, target, lease_generation: generation,
      method: string(data.method), params: object(data.params),
    };
    const oneWay = typeof command.params.reason === 'string' && ['browser.close', 'browser.control.release'].includes(command.method);
    void this.host.command(command).then(async value => {
      if (this.closed) return;
      if (!oneWay && !await this.complete(requestId, target, generation, value, null)) {
        if(this.host.retireLostReply(id,generation))this.update(id, { controlled: false });
        return;
      }
      if (command.method === 'browser.control.acquire') this.update(id, { controlled: true });
      if (command.method === 'browser.control.release' && object(value).released!==false) this.update(id, { controlled: false });
      if (command.method === 'browser.close') this.update(id, { state: 'closed', controlled: false });
      if (command.method === 'browser.navigate') this.update(id, { url: string(command.params.url) });
    }, async error => {
      if (!oneWay) await this.complete(requestId, target, generation, null, error instanceof Error ? error.message.slice(0, 128) : 'display_failed');
    });
    return true;
  }

  status(id: string): Status | null {
    const status = this.states.get(id);
    if (!status) return null;
    if (status.state !== 'ready') return status;
    try {
      const metadata = this.host.metadata(id);
      return { ...status, url: metadata.url, title: metadata.title, target: this.targets.get(id), controlled: metadata.control.state === 'acquired' };
    } catch { return { ...status, state: 'closed', controlled: false }; }
  }
  bounds(id: string, bounds: Rectangle | null): void {
    const target = this.targets.get(id);
    this.host.bounds(id, target && this.visibleTargets.has(key(target)) ? bounds : null);
  }

  async navigate(id: string, action: 'navigate' | 'back' | 'forward' | 'reload', url?: string): Promise<void> {
    const target = this.targets.get(id);
    if (!target || !this.visibleTargets.has(key(target)) || this.states.get(id)?.state !== 'ready') throw new Error('Browser is not ready');
    await this.host.navigateHuman(id, action, url);
  }

  async takeover(id: string): Promise<void> {
    await Promise.all([this.host.takeControl(id,false),
      this.core.request({ type: 'browser_takeover', id: crypto.randomUUID(), browser_id: id })]);
    this.update(id, { controlled: false });
  }

  async dispose(): Promise<void> { this.closed = true; await this.host.dispose(); this.states.clear(); this.targets.clear(); }

  private target(value: unknown): Target | null {
    return isRequest({ type: 'focus', id: 'native-browser', target: value }) ? value as Target : null;
  }

  private async cdpMessage(browser_id: string, lease_generation: string, raw: string): Promise<void> {
    const original = this.targets.get(browser_id);
    if (!original) throw new Error('route_changed');
    const target = { ...original };
    const check = (): void => {
      const current = this.targets.get(browser_id);
      if (this.closed || !current || !sameTarget(current, target)) throw new Error('route_changed');
      const metadata = this.host.metadata(browser_id);
      if (metadata.lease_generation !== lease_generation || metadata.control.state !== 'acquired') throw new Error('control_revoked');
    };
    check();
    const bytes = Buffer.byteLength(raw);
    if (!bytes || bytes > MAX_CDP_BYTES || this.cdpBytes + bytes > MAX_CDP_BYTES || this.cdpMessages >= MAX_CDP_MESSAGES) throw new Error('limit_exceeded');
    this.cdpBytes += bytes;
    this.cdpMessages++;
    try {
      const buffer = Buffer.from(raw, 'utf8');
      const endOfChunk = (start: number): number => {
        let end = Math.min(start + MAX_CDP_EVENT_CHUNK_BYTES, buffer.length);
        while (end < buffer.length && (buffer[end]! & 0xc0) === 0x80) end--;
        return end;
      };
      let total = 0;
      for (let start = 0; start < buffer.length; start = endOfChunk(start)) total++;
      const transfer = crypto.randomUUID().replaceAll('-', '');
      let index = 0;
      for (let start = 0; start < buffer.length;) {
        check();
        const end = endOfChunk(start);
        // One awaited native request at a time per relay, with fences between every chunk.
        // The relay's bounded FIFO retains the raw message until all acknowledgments arrive.
        const result = await this.core.request({ type: 'browser_cdp_event', id: crypto.randomUUID(), browser_id, target, lease_generation,
          chunk: { transfer, index, total, text: buffer.subarray(start, end).toString('utf8') } });
        check();
        if (object(result).accepted !== true) throw new Error('control_revoked');
        index++;
        start = end;
      }
    } finally { this.cdpBytes -= bytes; this.cdpMessages--; }
  }

  private async complete(id: string, target: Target, lease_generation: string, value: unknown, error: string | null): Promise<boolean> {
    if (this.closed) return false;
    try {
      const result = await this.core.request({ type: 'browser_result', id, target, lease_generation, value: value ?? null, error });
      return object(result).accepted === true;
    } catch { return false; }
  }

  private update(id: string, patch: Partial<Status>): void {
    const previous = this.states.get(id);
    if (!previous) return;
    const status = { ...previous, ...patch };
    if (previous.state === 'failed' && status.state === 'closed') status.state = 'failed';
    this.states.set(id, status);
    this.publish({ type: 'effect', kind: 'browser_status', data: this.status(id) ?? status });
    for (const [key, value] of this.states) {
      if (this.states.size <= 32) break;
      if (value.state === 'closed' || value.state === 'failed') { this.states.delete(key); this.targets.delete(key); }
    }
  }
}

function values(value: unknown): unknown[] { return Array.isArray(value) ? value : []; }
function key(target: Target): string { return JSON.stringify([target.generation, target.workspace_id, target.tab_id, target.pane_id, target.terminal_id]); }
function object(value: unknown): Record<string, unknown> { return value && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : {}; }
function string(value: unknown): string { return typeof value === 'string' ? value : ''; }
