import { randomBytes } from 'node:crypto';
import { BrowserUploads } from './browser-uploads.js';
import { BrowserDownloads } from './browser-downloads.js';
import { BrowserFileBudget } from './browser-file-budget.js';
import {
  allowedUrl, cdpChunk, cdpPageParams, fail, fields, integer, MAX_CDP_BYTES, MAX_CDP_EVENT_MESSAGES, MAX_CDP_MESSAGES,
  record, text,
} from './browser-policy.js';

interface RelayHost {
  check(): void;
  send(method: string, params: Record<string, unknown>, sessionId: string | undefined, timeout: number): Promise<Record<string, any>>;
  cleanup(method: string, params: Record<string, unknown>, sessionId?: string): void;
  describe(): { url: string; title: string; userAgent: string };
  contexts(): Record<string, unknown>[];
  reserveOutput(bytes: number): void;
  releaseOutput(bytes: number): void;
  write(raw: string): Promise<void>;
  revoke(reason: string): void;
}
interface Command { id: number; method: string; params: Record<string, unknown>; sessionId?: string }
interface Transfer { total: number; next: number; parts: string[]; bytes: number; deadline: number; timer: ReturnType<typeof setTimeout> }
interface SyntheticSession { kind: 'browser' | 'page'; parent?: string; announced: boolean; automatic: boolean; runtimeEnabled: boolean }
interface ChildSession { parent: string; owner: string; info: Record<string, unknown>; detaching: boolean }
interface Reply { result: unknown; after?: () => void }
interface Outgoing { raw: string; bytes: number; commandKey?: string }

const MAX_SYNTHETIC_SESSIONS = 8;
const MAX_CHILD_SESSIONS = 24;
const MAX_TRANSFERS_PER_LEASE = 65536;
const EVENT_DOMAINS = new Set(['Page', 'DOM', 'Runtime', 'Accessibility', 'Input', 'Network', 'Emulation', 'Fetch', 'Log', 'Performance', 'Inspector']);
const id = (): string => randomBytes(16).toString('hex');
const route = (sessionId?: string): Record<string, string> => sessionId ? { sessionId } : {};
function identifier(value: unknown): string {
  const result = text(value, 256);
  if (!/^[a-zA-Z0-9_.:-]+$/.test(result)) fail('invalid_params');
  return result;
}
function boolean(value: unknown): boolean { if (typeof value !== 'boolean') fail('invalid_params'); return value; }
function filter(value: unknown): void {
  if (value === undefined) return;
  if (!Array.isArray(value) || value.length > 16) fail('invalid_params');
  for (const entry of value) {
    const item = record(entry);
    fields(item, ['type', 'exclude']);
    if (item.type !== undefined) text(item.type, 64);
    if (item.exclude !== undefined) boolean(item.exclude);
  }
}
function includesType(value: unknown, type: string): boolean {
  if (value === undefined) return true;
  for (const entry of value as Array<Record<string, unknown>>) {
    if (entry.type === undefined || entry.type === type) return entry.exclude !== true;
  }
  return false;
}

// Check before stringify as well as afterwards: a debugger event can contain large page data.
// Conservative escape accounting and depth/node limits avoid unbounded serialization work.
function encode(value: unknown): string {
  let budget = MAX_CDP_BYTES;
  let nodes = 0;
  const visit = (item: unknown, depth: number): void => {
    if (++nodes > 1_000_000 || depth > 128) fail('limit_exceeded');
    if (typeof item === 'string') {
      budget -= Buffer.byteLength(item) + 3;
      for (const _match of item.matchAll(/["\\\u0000-\u001f\ud800-\udfff]/g)) {
        budget -= 5;
        if (budget < 0) fail('limit_exceeded');
      }
    } else if (item && typeof item === 'object') {
      budget -= 2;
      if (Array.isArray(item)) for (const child of item) visit(child, depth + 1);
      else for (const [key, child] of Object.entries(item)) { visit(key, depth + 1); visit(child, depth + 1); }
    } else budget -= 26;
    if (budget < 0) fail('limit_exceeded');
  };
  visit(value, 0);
  const raw = JSON.stringify(value);
  if (raw === undefined || Buffer.byteLength(raw) > MAX_CDP_BYTES) fail('limit_exceeded');
  return raw;
}

/** One lease, one guest, the host's existing debugger. There is no browser-wide CDP socket. */
export class BrowserCdp {
  private readonly uploads: BrowserUploads;
  private readonly downloads: BrowserDownloads;
  private cleaned?: Promise<void>;
  private closed = false;
  private ready = false;
  private pageId = '';
  private readonly browserId = `luvus-browser-${id()}`;
  private readonly contextId = `luvus-context-${id()}`;
  private readonly sessions = new Map<string, SyntheticSession>();
  private readonly children = new Map<string, ChildSession>();
  private readonly retiredChildren = new Map<string, ChildSession>();
  // Real debugger parent -> the synthetic page/validated child that requested auto-attach.
  private readonly autoAttach = new Map<string, string>();
  private readonly discovering = new Set<string>();
  private readonly transfers = new Map<string, Transfer>();
  private readonly usedTransfers = new Set<string>();
  private incomingBytes = 0; // Assemblers plus in-flight parsed commands, not just their last chunks.
  private readonly commands = new Set<string>();
  private readonly output: Outgoing[] = [];
  private outputBytes = 0;
  private draining = false;
  private writing?: Outgoing;

  constructor(private readonly host: RelayHost, budget: BrowserFileBudget) {
    this.uploads = new BrowserUploads(budget);
    this.downloads = new BrowserDownloads(() => this.check(), raw => host.write(raw), () => this.fatal('control_revoked'), budget);
  }

  acceptDownload(item: Electron.DownloadItem): boolean {
    return this.ready && !this.closed && this.downloads.accept(item, this.pageId);
  }

  async initialize(timeout: number): Promise<void> {
    this.check();
    // Target.getTargetInfo without an ID is scoped to this debugger attachment. Older Electron
    // versions may not support it; Chromium's main frame ID is the page target ID as a fallback.
    let info: Record<string, any> | undefined;
    try { info = (await this.host.send('Target.getTargetInfo', {}, undefined, timeout)).targetInfo; }
    catch { this.check(); }
    this.check();
    if (info?.type === 'page') this.pageId = identifier(info.targetId);
    else {
      const tree = await this.host.send('Page.getFrameTree', {}, undefined, timeout);
      this.pageId = identifier(tree.frameTree?.frame?.id);
    }
    this.check();
    this.host.contexts();
    this.ready = true;
  }

  /** Synchronous admission only. CDP replies/events use the separately backpressured stream. */
  async receive(value: unknown, timeout: number): Promise<void> {
    this.check();
    if (!this.ready) fail('control_denied');
    try {
      const chunk = cdpChunk(value);
      let transfer = this.transfers.get(chunk.transfer);
      if (!transfer) {
        if (chunk.index !== 0 || this.usedTransfers.has(chunk.transfer)) fail('invalid_params');
        if (this.transfers.size >= 2 || this.usedTransfers.size >= MAX_TRANSFERS_PER_LEASE) fail('limit_exceeded');
        this.usedTransfers.add(chunk.transfer);
        transfer = { total: chunk.total, next: 0, parts: [], bytes: 0, deadline: performance.now() + timeout,
          timer: setTimeout(() => this.fatal('timeout'), timeout) };
        this.transfers.set(chunk.transfer, transfer);
      }
      if (transfer.total !== chunk.total || transfer.next !== chunk.index) fail('invalid_params');
      if (performance.now() >= transfer.deadline) fail('timeout');
      const bytes = Buffer.byteLength(chunk.text);
      if (this.incomingBytes + bytes > MAX_CDP_BYTES) fail('limit_exceeded');
      this.incomingBytes += bytes;
      transfer.bytes += bytes;
      transfer.parts.push(chunk.text);
      transfer.next++;
      if (transfer.next !== transfer.total) return;
      clearTimeout(transfer.timer);
      this.transfers.delete(chunk.transfer);
      const envelope = record(JSON.parse(transfer.parts.join('')));
      if (Object.hasOwn(envelope, 'stage')) {
        fields(envelope, ['stage']);
        await this.uploads.append(envelope.stage, () => this.check());
        this.check();
        this.incomingBytes -= transfer.bytes;
        return;
      }
      const command = this.parse(envelope);
      const key = JSON.stringify([command.sessionId ?? '', command.id]);
      if (this.commands.has(key)) fail('invalid_params');
      if (this.commands.size >= MAX_CDP_MESSAGES) fail('limit_exceeded');
      this.commands.add(key);
      // The set is a hard bound on asynchronous work. Slow Runtime promises must not block
      // dialog handling or subsequent input on the same CDP connection.
      void this.run(command, key, transfer.bytes, Math.max(1, Math.ceil(transfer.deadline - performance.now())));
    } catch (error) {
      this.fatal(error instanceof Error ? error.message : 'invalid_params');
      throw error;
    }
  }

  private parse(value: Record<string, unknown>): Command {
    fields(value, ['id', 'method', 'params', 'sessionId']);
    const command: Command = { id: integer(value.id, 0, Number.MAX_SAFE_INTEGER), method: text(value.method, 128), params: {} };
    if (!/^[A-Za-z]+\.[A-Za-z][A-Za-z0-9]*$/.test(command.method)) fail('invalid_params');
    if (value.params !== undefined) command.params = record(value.params);
    if (value.sessionId !== undefined) {
      command.sessionId = identifier(value.sessionId);
      if (!this.sessions.has(command.sessionId) && !this.children.has(command.sessionId)) fail('invalid_params');
    }
    return command;
  }

  private async run(command: Command, key: string, bytes: number, timeout: number): Promise<void> {
    try {
      this.check();
      const reply = await this.execute(command, timeout);
      this.check();
      this.emit({ id: command.id, result: reply.result ?? {}, ...route(command.sessionId) }, key);
      // Once the reply is queued, a failed announcement must close the scope, never produce
      // a second response for that ID or keep a silently incomplete session inventory.
      if (!this.closed) {
        try { reply.after?.(); }
        catch { this.fatal('control_revoked'); }
      }
    } catch (error) {
      if (!this.closed) {
        if (error instanceof Error && ['timeout', 'limit_exceeded', 'control_revoked', 'browser_closed'].includes(error.message)) this.fatal(error.message);
        else this.emit({ id: command.id, error: { code: -32000, message: error instanceof Error && error.message === 'invalid_params' ? 'invalid_params' : 'unsupported' }, ...route(command.sessionId) }, key);
      }
    } finally {
      // IDs remain reserved until their response has passed native backpressure.
      if (!this.closed) this.incomingBytes -= bytes;
    }
  }

  private pageInfo(): Record<string, unknown> {
    const info = this.host.describe();
    return { targetId: this.pageId, type: 'page', title: info.title, url: allowedUrl(info.url) ? info.url : 'about:blank', attached: true, canAccessOpener: false, browserContextId: this.contextId };
  }
  private browserInfo(): Record<string, unknown> {
    return { targetId: this.browserId, type: 'browser', title: '', url: '', attached: true, canAccessOpener: false };
  }
  private newSession(kind: 'browser' | 'page', parent?: string, automatic = false): string {
    if (this.sessions.size >= MAX_SYNTHETIC_SESSIONS) fail('limit_exceeded');
    const sessionId = `luvus-${kind}-${id()}`;
    this.sessions.set(sessionId, { kind, parent, automatic, announced: kind === 'browser', runtimeEnabled: false });
    return sessionId;
  }
  private announce(sessionId: string): void {
    const session = this.sessions.get(sessionId);
    if (!session || session.announced) return;
    session.announced = true;
    this.emit({ method: 'Target.attachedToTarget', params: { sessionId, targetInfo: this.pageInfo(), waitingForDebugger: false }, ...route(session.parent) });
  }

  private async execute(command: Command, timeout: number): Promise<Reply> {
    const { method, params, sessionId } = command;
    const deadline = performance.now() + timeout;
    const send = (method: string, params: Record<string, unknown>, sessionId?: string): Promise<Record<string, any>> => {
      this.check();
      const remaining = Math.ceil(deadline - performance.now());
      if (remaining <= 0) fail('timeout');
      return this.host.send(method, params, sessionId, remaining);
    };
    const synthetic = sessionId ? this.sessions.get(sessionId) : undefined;
    const child = sessionId ? this.children.get(sessionId) : undefined;
    if (sessionId && !synthetic && !child || child?.detaching) fail('invalid_params');
    const browser = !sessionId || synthetic?.kind === 'browser';
    if (method === 'Luvus.setDownloadBehavior') {
      if (!browser) fail('unsupported');
      fields(params, ['permit', 'behavior', 'browserContextId', 'eventsEnabled']);
      if (params.browserContextId !== undefined && params.browserContextId !== this.contextId) fail('unsupported');
      if (params.eventsEnabled !== undefined) boolean(params.eventsEnabled);
      if (params.permit === null ? !['default', 'deny'].includes(String(params.behavior)) :
        !['allow', 'allowAndName'].includes(String(params.behavior)) || typeof params.permit !== 'string' || !/^[a-f0-9]{32}$/.test(params.permit)) fail('invalid_params');
      await this.downloads.configure(params.permit as string | null, sessionId);
      return { result: {} };
    }
    if (method === 'DOM.setFileInputFiles' || method === 'Page.setDownloadBehavior') fail('unsupported');
    if (method === 'Luvus.setFileInputFiles') {
      if (browser || child?.info.type === 'worker') fail('unsupported');
      fields(params, ['files', 'objectId', 'nodeId', 'backendNodeId']);
      const targets = ['objectId', 'nodeId', 'backendNodeId'].filter(key => params[key] !== undefined);
      if (targets.length !== 1) fail('invalid_params');
      const target = targets[0]!;
      const node = target === 'objectId' ? text(params[target], 512) : integer(params[target], 1, Number.MAX_SAFE_INTEGER);
      const paths = this.uploads.paths(params.files);
      const peer = child ? sessionId : undefined;
      if (paths.length) return { result: await send('DOM.setFileInputFiles', { [target]: node, files: paths }, peer) };
      // Chromium treats an empty paths vector as a cancelled chooser. Clear the
      // addressed input explicitly so callers do not submit the previous file.
      const objectId = target === 'objectId' ? node :
        (await send('DOM.resolveNode', { [target]: node }, peer)).object?.objectId;
      if (typeof objectId !== 'string') fail('invalid_params');
      try {
        const result = await send('Runtime.callFunctionOn', { objectId, returnByValue: true,
          functionDeclaration: `function () {
            if (!(this instanceof HTMLInputElement) || this.type !== 'file') return false;
            if (this.files.length) {
              this.value = '';
              this.dispatchEvent(new Event('input', { bubbles: true }));
              this.dispatchEvent(new Event('change', { bubbles: true }));
            }
            return true;
          }` }, peer);
        if (result.result?.value !== true) fail('invalid_params');
        return { result: {} };
      } finally { if (target !== 'objectId') this.host.cleanup('Runtime.releaseObject', { objectId }, peer); }
    }
    if (method === 'Target.getTargetInfo') {
      fields(params, ['targetId']);
      if (params.targetId !== undefined) {
        if (params.targetId === this.pageId) return { result: { targetInfo: this.pageInfo() } };
        if (params.targetId === this.browserId && browser) return { result: { targetInfo: this.browserInfo() } };
        if (child && params.targetId === child.info.targetId) return { result: { targetInfo: child.info } };
        fail('unsupported');
      }
      return { result: { targetInfo: browser ? this.browserInfo() : child?.info ?? this.pageInfo() } };
    }
    if (method === 'Target.setAutoAttach' && !browser) {
      fields(params, ['autoAttach', 'waitForDebuggerOnStart', 'flatten', 'filter']);
      const enabled = boolean(params.autoAttach);
      boolean(params.waitForDebuggerOnStart);
      if (params.flatten !== true) fail('unsupported');
      filter(params.filter);
      const parent = child ? sessionId! : '';
      const previous = this.autoAttach.get(parent);
      if (previous && previous !== sessionId) fail('control_denied');
      // The real page can only auto-attach its related targets. Never send browser-level discovery.
      // Filter further to dedicated workers/iframes; shared/service workers are not tab-owned.
      if (enabled) this.autoAttach.set(parent, sessionId!);
      try {
        const result = await send(method, { autoAttach: enabled, waitForDebuggerOnStart: params.waitForDebuggerOnStart,
          flatten: true, filter: [{ type: 'iframe', exclude: !includesType(params.filter, 'iframe') },
            { type: 'worker', exclude: !includesType(params.filter, 'worker') }, { exclude: true }] }, parent || undefined);
        if (!enabled) this.autoAttach.delete(parent);
        return { result };
      } catch (error) {
        if (previous) this.autoAttach.set(parent, previous); else this.autoAttach.delete(parent);
        throw error;
      }
    }
    if (method === 'Target.detachFromTarget') {
      fields(params, ['sessionId']);
      const detached = identifier(params.sessionId);
      const nested = this.children.get(detached);
      if (nested) {
        if (nested.detaching) fail('invalid_params');
        nested.detaching = true;
        let result: Record<string, any>;
        try { result = await send(method, { sessionId: detached }, nested.parent || undefined); }
        catch (error) { nested.detaching = false; throw error; }
        return { result };
      }
      if (!this.sessions.has(detached)) fail('invalid_params');
      return { result: {}, after: () => this.detachSynthetic(detached) };
    }
    if (browser) return this.browserCommand(command);
    cdpPageParams(method, params);
    if (synthetic && method === 'Runtime.enable') {
      fields(params, []);
      return { result: {}, after: () => {
        if (synthetic.runtimeEnabled || !this.sessions.has(sessionId!)) return;
        synthetic.runtimeEnabled = true;
        for (const context of this.host.contexts()) this.emit({ method: 'Runtime.executionContextCreated', params: { context }, ...route(sessionId) });
      } };
    }
    if (synthetic && method === 'Runtime.disable') {
      fields(params, []);
      synthetic.runtimeEnabled = false;
      return { result: {} };
    }
    // These domains are also used by the high-level tools. A relay must not disable their
    // host subscriptions or allow a native file chooser to escape the upload policy.
    if (['Page.disable', 'DOM.disable', 'Page.bringToFront'].includes(method)
        || method === 'Runtime.runIfWaitingForDebugger' && !child) return { result: {} };
    if (method === 'Page.createIsolatedWorld') {
      fields(params, ['frameId', 'worldName', 'grantUniveralAccess']);
      const frameId = identifier(params.frameId);
      if (params.worldName !== undefined) text(params.worldName, 256, true);
      if (params.grantUniveralAccess !== undefined) boolean(params.grantUniveralAccess);
      const tree = await send('Page.getFrameTree', {}, child ? sessionId : undefined);
      const frames = [tree.frameTree];
      let owned = false;
      for (let index = 0; index < frames.length; index++) {
        if (frames.length > 1024) fail('limit_exceeded');
        const frame = frames[index];
        if (frame?.frame?.id === frameId) owned = true;
        if (Array.isArray(frame?.childFrames)) {
          if (frames.length + frame.childFrames.length > 1024) fail('limit_exceeded');
          frames.push(...frame.childFrames);
        }
      }
      if (!owned) fail('unsupported');
    }
    if (method === 'Page.navigateToHistoryEntry') {
      fields(params, ['entryId']);
      integer(params.entryId, 0, Number.MAX_SAFE_INTEGER);
      const history = await send('Page.getNavigationHistory', {}, child ? sessionId : undefined);
      const entry = Array.isArray(history.entries) ? history.entries.find((item: Record<string, unknown>) => item.id === params.entryId) : undefined;
      if (!entry || !allowedUrl(entry.url)) fail('unsupported');
    }
    return { result: await send(method, params, child ? sessionId : undefined) };
  }

  private async browserCommand({ method, params, sessionId }: Command): Promise<Reply> {
    switch (method) {
      case 'Browser.getVersion':
        fields(params, []);
        return { result: { protocolVersion: '1.3', product: `Chrome/${process.versions.chrome ?? ''}`, revision: '',
          userAgent: this.host.describe().userAgent, jsVersion: process.versions.v8 } };
      case 'Target.getTargets':
        fields(params, ['filter']); filter(params.filter);
        return { result: { targetInfos: includesType(params.filter, 'page') ? [this.pageInfo()] : [] } };
      case 'Target.getBrowserContexts': fields(params, []); return { result: { browserContextIds: [] } };
      case 'Target.attachToBrowserTarget':
        fields(params, []);
        return { result: { sessionId: this.newSession('browser', sessionId) } };
      case 'Target.attachToTarget': {
        fields(params, ['targetId', 'flatten']);
        if (params.targetId !== this.pageId || params.flatten !== true) fail('unsupported');
        const attached = this.newSession('page', sessionId);
        return { result: { sessionId: attached }, after: () => this.announce(attached) };
      }
      case 'Target.setDiscoverTargets': {
        fields(params, ['discover', 'filter']); filter(params.filter);
        const discover = boolean(params.discover) && includesType(params.filter, 'page');
        const scope = sessionId ?? '';
        const previous = this.discovering.has(scope);
        if (discover) this.discovering.add(scope); else this.discovering.delete(scope);
        return { result: {}, after: () => {
          if (discover && !previous) this.emit({ method: 'Target.targetCreated', params: { targetInfo: this.pageInfo() }, ...route(sessionId) });
        } };
      }
      case 'Target.setAutoAttach': {
        fields(params, ['autoAttach', 'waitForDebuggerOnStart', 'flatten', 'filter']);
        const enabled = boolean(params.autoAttach);
        boolean(params.waitForDebuggerOnStart);
        if (params.flatten !== true) fail('unsupported');
        filter(params.filter);
        const existing = [...this.sessions].find(([, value]) => value.kind === 'page' && value.automatic && value.parent === sessionId)?.[0];
        if (!enabled || !includesType(params.filter, 'page')) return { result: {}, after: () => { if (existing) this.detachSynthetic(existing); } };
        if (existing) return { result: {} };
        const attached = this.newSession('page', sessionId, true);
        return { result: {}, after: () => this.announce(attached) };
      }
      case 'Browser.setDownloadBehavior':
        fields(params, ['behavior', 'eventsEnabled']);
        if (params.behavior !== 'deny') fail('unsupported');
        if (params.eventsEnabled !== undefined) boolean(params.eventsEnabled);
        // Only the owner gateway can arm a file permit. A raw relay may turn it off.
        await this.downloads.configure(null);
        return { result: {} };
      default: fail('unsupported');
    }
  }

  /** Only Electron debugger events may introduce a real descendant session. */
  event(method: string, value: unknown, source?: string): void {
    if (this.closed) return;
    try {
      this.check();
      const parent = source || '';
      // A detached parent can be reported before its descendants. Their bounded tombstones
      // still validate the original parent route; no new session can enter through them.
      if (parent && !this.children.has(parent) && method !== 'Target.detachedFromTarget') fail('invalid_params');
      const params = record(value);
      if (method === 'Target.attachedToTarget') {
        const owner = this.autoAttach.get(parent);
        if (!owner) fail('invalid_params');
        const sessionId = identifier(params.sessionId);
        const info = record(params.targetInfo);
        identifier(info.targetId);
        boolean(params.waitingForDebugger);
        if (!['iframe', 'worker'].includes(String(info.type))) fail('unsupported');
        const url = text(info.url, 8192, true);
        if (url && !allowedUrl(url) && !(info.type === 'worker' && /^blob:https?:\/\//.test(url))) fail('unsupported');
        if (this.children.has(sessionId) || this.sessions.has(sessionId) || this.retiredChildren.has(sessionId)
            || info.targetId === this.pageId || [...this.children.values()].some(child => child.info.targetId === info.targetId)) fail('invalid_params');
        if (this.children.size >= MAX_CHILD_SESSIONS) fail('limit_exceeded');
        const safeInfo = { targetId: info.targetId, type: info.type, title: text(info.title, 8192, true), url,
          attached: true, canAccessOpener: false, browserContextId: this.contextId,
          ...(info.parentFrameId === undefined ? {} : { parentFrameId: identifier(info.parentFrameId) }) };
        this.children.set(sessionId, { parent, owner, info: safeInfo, detaching: false });
        this.emit({ method, params: { sessionId, targetInfo: safeInfo, waitingForDebugger: params.waitingForDebugger }, ...route(owner) });
        return;
      }
      if (method === 'Target.detachedFromTarget') {
        const sessionId = identifier(params.sessionId);
        const child = this.children.get(sessionId) ?? this.retiredChildren.get(sessionId);
        if (!child || child.parent !== parent || params.targetId !== undefined && params.targetId !== child.info.targetId) fail('invalid_params');
        if (this.retiredChildren.delete(sessionId)) return; // Already emitted when its parent detached.
        this.detachChild(sessionId, false);
        return;
      }
      if (method === 'Target.targetInfoChanged') {
        const info = record(params.targetInfo);
        if (info.targetId === this.pageId && !parent) { this.changed(); return; }
        const entry = [...this.children].find(([, child]) => child.info.targetId === info.targetId && child.parent === parent);
        if (!entry) fail('invalid_params');
        const [sessionId, child] = entry;
        const url = text(info.url, 8192, true);
        if (info.type !== child.info.type || url && !allowedUrl(url) && !(info.type === 'worker' && /^blob:https?:\/\//.test(url))) fail('unsupported');
        child.info = { ...child.info, title: text(info.title, 8192, true), url };
        this.emit({ method, params: { targetInfo: child.info }, ...route(child.owner) });
        this.children.set(sessionId, child);
        return;
      }
      if (!EVENT_DOMAINS.has(method.split('.')[0]!)) fail('unsupported');
      if (parent) this.emit({ method, params, sessionId: parent });
      else for (const [sessionId, session] of this.sessions) {
        if (session.kind === 'page' && session.announced && (!method.startsWith('Runtime.') || session.runtimeEnabled)) this.emit({ method, params, sessionId });
      }
    } catch (error) { this.fatal(error instanceof Error ? error.message : 'invalid_params'); }
  }

  changed(): void {
    if (this.closed || !this.ready) return;
    try {
      this.check();
      const targetInfo = this.pageInfo();
      const scopes = new Set(this.discovering);
      for (const session of this.sessions.values()) if (session.kind === 'page' && session.announced) scopes.add(session.parent ?? '');
      for (const scope of scopes) this.emit({ method: 'Target.targetInfoChanged', params: { targetInfo }, ...route(scope) });
    } catch { this.fatal('control_revoked'); }
  }

  private detachChild(sessionId: string, awaitingEvent = true): void {
    const child = this.children.get(sessionId);
    if (!child) return;
    for (const [id, nested] of [...this.children]) if (nested.parent === sessionId) this.detachChild(id);
    this.children.delete(sessionId);
    this.autoAttach.delete(sessionId);
    if (awaitingEvent) {
      if (this.retiredChildren.size >= 128) { this.fatal('limit_exceeded'); return; }
      this.retiredChildren.set(sessionId, child);
    }
    this.emit({ method: 'Target.detachedFromTarget', params: { sessionId, targetId: child.info.targetId }, ...route(child.owner) });
  }
  private detachSynthetic(sessionId: string): void {
    const session = this.sessions.get(sessionId);
    if (!session) return;
    for (const [id, nested] of [...this.sessions]) if (nested.parent === sessionId) this.detachSynthetic(id);
    for (const [parent, owner] of [...this.autoAttach]) if (owner === sessionId) {
      this.host.cleanup('Target.setAutoAttach', { autoAttach: false, waitForDebuggerOnStart: false, flatten: true }, parent || undefined);
      this.autoAttach.delete(parent);
    }
    for (const [id, child] of [...this.children]) if (child.owner === sessionId) {
      this.host.cleanup('Target.detachFromTarget', { sessionId: id }, child.parent || undefined);
      this.detachChild(id);
    }
    this.sessions.delete(sessionId);
    this.discovering.delete(sessionId);
    this.emit({ method: 'Target.detachedFromTarget', params: { sessionId, targetId: session.kind === 'page' ? this.pageId : this.browserId }, ...route(session.parent) });
  }

  private emit(message: unknown, commandKey?: string): void {
    if (this.closed) return;
    try {
      this.check();
      const raw = encode(message);
      const bytes = Buffer.byteLength(raw);
      if (this.output.length >= MAX_CDP_EVENT_MESSAGES || this.outputBytes + bytes > MAX_CDP_BYTES) fail('limit_exceeded');
      this.host.reserveOutput(bytes);
      this.output.push({ raw, bytes, commandKey });
      this.outputBytes += bytes;
      if (!this.draining) { this.draining = true; void this.drain(); }
    } catch (error) { this.fatal(error instanceof Error ? error.message : 'limit_exceeded'); }
  }
  private async drain(): Promise<void> {
    try {
      while (!this.closed && this.output.length) {
        this.check();
        const message = this.output[0]!; // Includes the in-flight message in both queue budgets.
        this.writing = message;
        try {
          await this.host.write(message.raw);
          this.check();
        } finally {
          // A revoked relay can still have one native request unwinding. Its memory remains
          // charged to the shared host budget until the callback actually settles.
          if (this.output[0] === message) { this.output.shift(); this.outputBytes -= message.bytes; }
          this.writing = undefined;
          if (message.commandKey !== undefined) this.commands.delete(message.commandKey);
          this.host.releaseOutput(message.bytes);
        }
      }
    } catch { this.fatal('control_revoked'); }
    finally { this.draining = false; }
  }
  private check(): void { if (this.closed) fail('control_revoked'); this.host.check(); }
  private fatal(reason: string): void {
    if (this.closed) return;
    this.dispose();
    this.host.revoke(['timeout', 'limit_exceeded'].includes(reason) ? reason : 'control_revoked');
  }

  dispose(): Promise<void> {
    if (this.closed) return this.cleaned ?? Promise.resolve();
    this.closed = true;
    for (const transfer of this.transfers.values()) clearTimeout(transfer.timer);
    this.transfers.clear();
    this.usedTransfers.clear();
    this.commands.clear();
    this.incomingBytes = 0;
    for (const message of this.output) if (message !== this.writing) this.host.releaseOutput(message.bytes);
    this.output.length = 0;
    this.outputBytes = 0;
    // Dispatch cleanup before a subsequent lease can attach, without detaching the shared
    // debugger needed by the high-level tools. Resume any paused descendants before release.
    for (const [sessionId, child] of [...this.children].reverse()) {
      this.host.cleanup('Runtime.runIfWaitingForDebugger', {}, sessionId);
      this.host.cleanup('Target.detachFromTarget', { sessionId }, child.parent || undefined);
    }
    for (const parent of this.autoAttach.keys()) this.host.cleanup('Target.setAutoAttach', { autoAttach: false, waitForDebuggerOnStart: false, flatten: true }, parent || undefined);
    // A paused Fetch request must not survive handover to a human or the next lease.
    this.host.cleanup('Fetch.disable', {});
    this.autoAttach.clear();
    this.children.clear();
    this.retiredChildren.clear();
    this.sessions.clear();
    this.discovering.clear();
    return this.cleaned = Promise.all([this.uploads.dispose(), this.downloads.dispose()]).then(() => {});
  }
}
