import { randomUUID } from 'node:crypto';
import { BrowserWindow, session, WebContentsView } from 'electron';
import type { Target } from '@luvus/app-protocol';
import {
  actionParams, allowedUrl, bounded, commandParams, fail, fields, generation, integer,
  MAX_CDP_BYTES, MAX_CDP_EVENT_MESSAGES, MAX_REFS, MAX_RESULT_BYTES, MAX_TIMEOUT_MS, record, sameTarget, shortText, text, validateTarget,
} from './browser-policy.js';
import { BrowserOperation, CANCEL_WAIT, NODE_ACTION, WAIT_FOR_PAGE } from './browser-tools.js';
import { BrowserCdp } from './browser-cdp.js';
import { BrowserFileBudget } from './browser-file-budget.js';

export interface BrowserDescriptor {
  id: string;
  url: string;
  proxy_port: number;
  generation: string;
  workspace_id: string;
  pane_id: string;
  target: Target;
  request_id: string;
  lease_generation: string;
}

/** Trusted owner-to-display command. Never contains a bearer lease or renderer credentials. */
export interface BrowserCommand {
  request_id: string;
  browser_id: string;
  method: string;
  params: Record<string, unknown>;
  lease_generation: string;
  target: Target;
}

interface Dialog { type: string; message: string; default_prompt: string }
export interface BrowserMetadata {
  browser_id: string;
  url: string;
  title: string;
  document_revision: string;
  dialog: Dialog | null;
  lease_generation: string;
  control: { state: 'acquired' | 'uncontrolled'; expires_at: number | null };
}

interface Lease { deadline: number; expiresAt: number; timer: ReturnType<typeof setTimeout> }
interface Context { id: number; uniqueId: string; frameId: string }
interface Tab {
  view: WebContentsView;
  browserSession: Electron.Session;
  descriptor: BrowserDescriptor;
  ready: boolean;
  debuggerReady: boolean;
  frameId?: string;
  generation: bigint;
  lease?: Lease;
  revision: bigint;
  navigating: boolean;
  refs: Map<string, number>;
  nextRef: number;
  contexts: Map<number, Context>;
  cdpContexts: Map<number, Record<string, unknown>>;
  cdpContextOverflow: boolean;
  isolatedContext?: Context;
  dialog: Dialog | null;
  pending: Set<BrowserOperation>;
  cdpPending: Set<BrowserOperation>;
  inputTail: Promise<void>;
  // At most one matching native hook may pass during each CDP dispatch. Electron does not
  // expose a trusted source marker here. Physical/synthetic overlap still needs App E2E coverage.
  input?: { kind: 'key' | 'mouse'; type: string; key?: string; x?: number; y?: number; modifiers?: number;
    consumed: boolean; observed: boolean; wheelGestures: Set<string> };
  cdp?: BrowserCdp;
  humanGesture?: {revision:bigint; generation:bigint; until:number};
  humanDownload?: Electron.DownloadItem;
  retiringFiles?:Promise<void>;
}
interface RuntimeReply {
  result?: { value?: unknown; type?: string; unserializableValue?: string; objectId?: string };
  exceptionDetails?: { exception?: { description?: string } };
}
interface AXNode {
  nodeId: string;
  backendDOMNodeId?: number;
  ignored?: boolean;
  role?: { value?: unknown };
  name?: { value?: unknown };
  value?: { value?: unknown };
  description?: { value?: unknown };
  childIds?: string[];
}

export class BrowserHost {
  private readonly fileBudget = new BrowserFileBudget();
  private readonly tabs = new Map<string, Tab>();
  private readonly retired = new Set<string>();
  private readonly closing = new Map<string, Promise<void>>();
  private readonly cleaning = new Set<Promise<void>>();
  private active: string | undefined;
  private cdpOutputBytes = 0;
  private cdpOutputMessages = 0;

  constructor(private readonly window: BrowserWindow,
    private readonly onControlRevoked?: (browserId: string, reason: string, generation:string) => void,
    private readonly onCdpMessage?: (browserId: string, leaseGeneration: string, raw: string) => Promise<void>,
    private readonly onMetadataChanged?: (browserId: string) => void) {}

  async open(input: BrowserDescriptor): Promise<void> {
    if (!/^[\w:.-]{1,128}$/.test(input.id)) fail('invalid_params');
    integer(input.proxy_port, 1, 65535);
    text(input.request_id, 128);
    validateTarget(input.target);
    const leaseGeneration = generation(input.lease_generation);
    if (input.generation !== input.target.generation || input.workspace_id !== input.target.workspace_id || input.pane_id !== input.target.pane_id) fail('route_changed');
    if (!allowedUrl(input.url)) fail('unsupported');
    if (this.retired.has(input.id)) fail('browser_closed');
    const existing = this.tabs.get(input.id);
    if (existing) {
      if (!sameTarget(existing.descriptor.target, input.target) || input.proxy_port !== existing.descriptor.proxy_port || input.request_id !== existing.descriptor.request_id) fail('route_changed');
      // A launch replay is not control acquisition or lease renewal.
      if (leaseGeneration !== existing.generation || !existing.ready) fail('control_revoked');
      this.show(input.id);
      return;
    }
    if (this.tabs.size >= 8 || this.retired.size >= 1024) fail('limit_exceeded');
    const descriptor = { ...input, target: { ...input.target } };
    const browserSession = session.fromPartition(`luvus-browser-${descriptor.id}`, { cache: false });
    browserSession.setPermissionRequestHandler((_contents, _permission, callback) => callback(false));
    browserSession.setPermissionCheckHandler(() => false);
    browserSession.setDevicePermissionHandler(() => false);
    let proxyReady = false;
    browserSession.webRequest.onBeforeRequest((details, callback) => {
      const navigation = details.resourceType === 'mainFrame' || details.resourceType === 'subFrame';
      const allowed = navigation ? allowedUrl(details.url) : /^(https?:|wss?:|data:|blob:)/.test(details.url);
      callback({ cancel: !proxyReady || !allowed });
    });
    const view = new WebContentsView({ webPreferences: { session: browserSession, nodeIntegration: false, contextIsolation: true, sandbox: true, webSecurity: true, navigateOnDragDrop: false } });
    const tab: Tab = { view, browserSession, descriptor, ready: false, debuggerReady: false, generation: leaseGeneration, revision: 0n, navigating: false,
      refs: new Map(), nextRef: 0, contexts: new Map(), cdpContexts: new Map(), cdpContextOverflow: false, dialog: null, pending: new Set(), cdpPending: new Set(), inputTail: Promise.resolve() };
    // Register before awaiting proxy setup or load, so close can cancel either phase.
    this.tabs.set(descriptor.id, tab);
    const contents = view.webContents;
    browserSession.on('will-download', (event, item, source) => {
      if (source !== contents) {event.preventDefault();return;}
      if (tab.cdp || this.controlled(tab)) {
        if (!tab.cdp?.acceptDownload(item)) event.preventDefault();
        return;
      }
      const gesture=tab.humanGesture;tab.humanGesture=undefined;
      if (!gesture || gesture.until<performance.now() || gesture.revision!==tab.revision || gesture.generation!==tab.generation ||
          tab.humanDownload || !this.humanVisible(tab)) {event.preventDefault();return;}
      const name=item.getFilename(),size=item.getTotalBytes(),url=item.getURL();
      if (!/^https?:/i.test(url) || !allowedUrl(url) || !name || name.length>200 || name.startsWith('.') ||
          /[\/\\:\u0000-\u001f\u007f]/.test(name) || size<=0 || size>64*1024*1024) {event.preventDefault();return;}
      // No fixed display path: Electron opens the native Save dialog. A CDP
      // denial or an untrusted filename can never enter this path.
      item.setSaveDialogOptions({title:'Save download from '+new URL(url).origin,defaultPath:name,showsTagField:false});
      tab.humanDownload=item;
      item.on('updated',()=>{if(item.getReceivedBytes()>64*1024*1024)item.cancel();});
      item.once('done',()=>{if(tab.humanDownload===item)tab.humanDownload=undefined;});
    });
    contents.setWebRTCIPHandlingPolicy('disable_non_proxied_udp');
    // Guest key injection must not invoke application-level menu accelerators.
    contents.setIgnoreMenuShortcuts(true);
    contents.setWindowOpenHandler(() => ({ action: 'deny' }));
    contents.on('will-navigate', (event, url) => { if (!allowedUrl(url)) event.preventDefault(); });
    contents.on('will-frame-navigate', event => { if (!allowedUrl(event.url)) event.preventDefault(); });
    contents.on('will-redirect', (event, url) => { if (!allowedUrl(url)) event.preventDefault(); });
    contents.on('will-attach-webview', event => event.preventDefault());
    contents.on('did-start-navigation', (_event, _url, _inPlace, main) => {
      if (main) { tab.navigating = true; this.invalidateDocument(tab); }
    });
    contents.on('did-navigate', (_event, url) => {
      this.invalidateDocument(tab);
      tab.navigating = false;
      if (!allowedUrl(url)) { this.revoke(tab, 'route_changed', true); void this.close(descriptor.id); }
    });
    contents.on('did-navigate-in-page', (_event, _url, main) => {
      if (main) { tab.navigating = false; this.invalidateDocument(tab); }
    });
    contents.on('did-fail-load', (_event, _code, _description, _url, main) => { if (main) tab.navigating = false; });
    contents.on('before-input-event', (event, input) => {
      if (!this.controlled(tab)) {
        if (input.type==='keyDown' && ['Enter',' '].includes(input.key)) this.noteHumanGesture(tab);
        return;
      }
      const expected = tab.input;
      const modifiers = (input.alt ? 1 : 0) | (input.control ? 2 : 0) | (input.meta ? 4 : 0) | (input.shift ? 8 : 0);
      if (expected?.kind === 'key' && !expected.consumed && expected.type === input.type && expected.key === input.key && expected.modifiers === modifiers) expected.consumed = true;
      else event.preventDefault();
    });
    contents.on('before-mouse-event', (event, input) => {
      if (!this.controlled(tab)) {
        if(input.type==='mouseDown' && input.button==='left')this.noteHumanGesture(tab);return;
      }
      const expected = tab.input;
      // OOPIF events are frame-local; project CDP coordinates through the current view position.
      const windowBounds = this.window.getContentBounds();
      const viewBounds = tab.view.getBounds();
      if (expected?.kind === 'mouse' && !expected.consumed && expected.type === input.type
          && typeof input.globalX === 'number' && typeof input.globalY === 'number'
          && Math.fround(windowBounds.x + viewBounds.x + expected.x!) === Math.fround(input.globalX)
          && Math.fround(windowBounds.y + viewBounds.y + expected.y!) === Math.fround(input.globalY)) expected.consumed = true;
      else event.preventDefault();
    });
    contents.on('input-event', (_event, input) => {
      if (!/^(mouseWheel|touch|gesture)/i.test(input.type) || !this.controlled(tab)) return;
      const expected = tab.input;
      // InputEvent has no trusted physical/synthetic marker (or guaranteed coordinates).
      // Admit just one wheel and its bounded scroll sequence during the authorized dispatch,
      // not all input while any CDP command is pending. Unmatched physical input revokes.
      if (expected?.kind === 'mouse' && expected.type === 'mouseWheel') {
        if (input.type === 'mouseWheel' && !expected.observed) { expected.observed = true; return; }
        if (expected.observed && ['gestureScrollBegin', 'gestureScrollUpdate', 'gestureScrollEnd'].includes(input.type)
            && !expected.wheelGestures.has(input.type)) { expected.wheelGestures.add(input.type); return; }
      }
      this.revoke(tab, 'control_revoked', true);
    });
    contents.on('devtools-opened', () => this.revoke(tab, 'control_revoked', true, true));
    contents.on('render-process-gone', () => { this.revoke(tab, 'browser_closed', true); void this.close(descriptor.id); });
    contents.on('destroyed', () => { this.revoke(tab, 'browser_closed', true); void this.close(descriptor.id); });
    contents.debugger.on('detach', () => {
      tab.debuggerReady = false;
      tab.contexts.clear();
      tab.cdpContexts.clear();
      tab.cdpContextOverflow = false;
      tab.isolatedContext = undefined;
      this.revoke(tab, 'control_revoked', true, true);
      this.invalidateDocument(tab);
    });
    contents.on('page-title-updated', () => tab.cdp?.changed());
    contents.on('did-navigate', () => { tab.cdp?.changed(); this.onMetadataChanged?.(descriptor.id); });
    contents.on('did-navigate-in-page', (_event, _url, main) => { if (main) { tab.cdp?.changed(); this.onMetadataChanged?.(descriptor.id); } });
    contents.debugger.on('message', (_event, method, params, sessionId) => {
      if (!sessionId) this.cacheCdpContext(tab, method, params);
      tab.cdp?.event(method, params, sessionId);
      // Child frame/worker contexts must not overwrite the high-level tools' page context.
      if (sessionId) return;
      if (method === 'Page.frameNavigated' && !params.frame?.parentId) {
        tab.frameId = params.frame?.id;
      } else if (method === 'Runtime.executionContextCreated') {
        const context = params.context;
        if (context?.auxData?.frameId === tab.frameId && typeof context.uniqueId === 'string') {
          const identity = { id: context.id, uniqueId: context.uniqueId, frameId: context.auxData.frameId };
          if (context.auxData.isDefault) { tab.contexts.clear(); tab.contexts.set(context.id, identity); }
          else if (context.name === 'luvus-browser-tools') tab.isolatedContext = identity;
        }
      } else if (method === 'Runtime.executionContextDestroyed') {
        tab.contexts.delete(params.executionContextId);
        if (tab.isolatedContext?.id === params.executionContextId) tab.isolatedContext = undefined;
      } else if (method === 'Runtime.executionContextsCleared') { tab.contexts.clear(); tab.isolatedContext = undefined; }
      else if (method === 'Page.javascriptDialogOpening') {
        tab.dialog = { type: shortText(params.type, 32), message: shortText(params.message, 4096), default_prompt: shortText(params.defaultPrompt, 4096) };
      } else if (method === 'Page.javascriptDialogClosed') tab.dialog = null;
    });
    this.window.contentView.addChildView(view);
    view.setVisible(false);
    const operation = this.operation(tab, false, MAX_TIMEOUT_MS, () => this.alive(tab));
    try {
      await operation.wait(browserSession.setProxy({ mode: 'fixed_servers', proxyRules: `http://127.0.0.1:${descriptor.proxy_port}`, proxyBypassRules: '<-loopback>' }));
      proxyReady = true;
      // CDP commands cannot resolve until the guest has an initialized renderer.
      await operation.wait(contents.loadURL('about:blank'));
      await this.attach(tab, operation);
      operation.check();
      await operation.wait(contents.loadURL(descriptor.url));
      tab.ready = true;
      this.show(descriptor.id);
    } catch (error) {
      void this.close(descriptor.id);
      throw this.safeError(error);
    } finally { this.finish(tab, operation); }
  }

  metadata(id: string): BrowserMetadata {
    const tab = this.tabs.get(id);
    if (!tab) fail('browser_closed');
    this.alive(tab);
    const controlled = this.controlled(tab);
    return bounded({ browser_id: id, url: allowedUrl(tab.view.webContents.getURL()) ? tab.view.webContents.getURL() : 'about:blank',
      title: shortText(tab.view.webContents.getTitle(), 4096), document_revision: tab.revision.toString(),
      dialog: tab.dialog ? { ...tab.dialog } : null, lease_generation: tab.generation.toString(),
      control: { state: controlled ? 'acquired' : 'uncontrolled', expires_at: tab.lease?.expiresAt ?? null } });
  }

  /** Explicit host-UI handover. Keeps the page, but never silently reacquires control. */
  takeControl(id: string, notify=true): Promise<void> {
    const tab = this.tabs.get(id);
    if (tab) {
      if (!notify) tab.generation++;
      this.revoke(tab, 'control_revoked', notify, true);
      return tab.retiringFiles??Promise.resolve();
    }
    return Promise.resolve();
  }
  retireLostReply(id:string,generation:string):boolean {
    const tab=this.tabs.get(id);
    if (!tab || tab.generation.toString()!==generation || !tab.lease) return false;
    this.revoke(tab,'control_revoked',true);
    return true;
  }

  async command(input: BrowserCommand): Promise<unknown> {
    text(input.request_id, 128);
    text(input.browser_id, 128);
    validateTarget(input.target);
    const nextGeneration = generation(input.lease_generation);
    const tab = this.tabs.get(input.browser_id);
    if (!tab) {
      if (input.method === 'browser.close' && typeof input.params?.reason === 'string') return { closed: true };
      fail('browser_closed');
    }
    if (!sameTarget(tab.descriptor.target, input.target)) fail('route_changed');
    // Revocations are one-way and can arrive after local expiry/takeover or a failed command.
    // A late revocation from a retired lease cannot tear down a newer lease.
    if (['browser.control.release', 'browser.close'].includes(input.method) && input.params.reason !== undefined) {
      fields(record(input.params), ['reason']);
      text(input.params.reason, 128);
      if(input.method==='browser.control.release' && nextGeneration<tab.generation)return {released:false};
      // Closing the original browser lifetime is terminal, independent of
      // the last controller generation. No later tab may reuse this ID.
      if(nextGeneration>tab.generation)tab.generation=nextGeneration;
      this.revoke(tab, 'control_revoked', false);
      if (input.method === 'browser.close') void this.close(input.browser_id);
      return { released: true };
    }
    const cdpCommand = input.method === 'browser.cdp.connect' || input.method === 'browser.cdp.send';
    // Malformed traffic from the active CDP controller closes that scope; a stale sender
    // must not be able to revoke a different lease with malformed chunks.
    if (cdpCommand) this.assertLease(tab, nextGeneration);
    let params: Record<string, unknown>;
    try { params = structuredClone(commandParams(input.method, input.params)); }
    catch (error) { if (cdpCommand && tab.cdp) this.revoke(tab, 'control_revoked', true); throw this.safeError(error); }
    this.alive(tab);
    if (!tab.ready) fail('control_denied');
    if (input.method === 'browser.control.release') {
      // The broker advances generation *before* forwarding an explicit release, too.
      if (nextGeneration <= tab.generation) fail('control_revoked');
      tab.generation = nextGeneration;
      this.revoke(tab, 'control_revoked', false);
      return { released: true };
    }
    const acquire = input.method === 'browser.control.acquire';
    const ttl = Number(params.ttl_s ?? 300);
    const leaseDeadline = performance.now() + ttl * 1000;
    const expiresAt = Math.floor(Date.now() / 1000) + ttl;
    if (acquire) {
      const alreadyControlled = this.controlled(tab); // Expiry can advance the local high-water mark.
      if (nextGeneration <= tab.generation || alreadyControlled || tab.pending.size || tab.humanDownload) fail('control_denied');
      tab.humanGesture=undefined;
      if (tab.view.webContents.isDevToolsOpened()) fail('control_denied');
      tab.generation = nextGeneration;
    } else this.assertLease(tab, nextGeneration);
    if (input.method === 'browser.close') { void this.close(input.browser_id); return { closed: true }; }
    if (cdpCommand ? tab.pending.size >= 40 : tab.pending.size - tab.cdpPending.size >= 8) fail('limit_exceeded');
    const navigation = cdpCommand || ['browser.navigate', 'browser.back', 'browser.forward', 'browser.reload'].includes(input.method)
      || input.method === 'browser.wait' && record(params.condition).load === true;
    const revision = tab.revision;
    const operation = this.operation(tab, !navigation && !acquire, Number(params.timeout_ms ?? 10000), () => {
      this.alive(tab);
      if (tab.generation !== nextGeneration) fail('control_revoked');
      if (!acquire) this.assertLease(tab, nextGeneration);
      if (!navigation && !acquire && tab.revision !== revision) fail('stale_reference');
    });
    try {
      operation.check();
      if (acquire) {
        await this.attach(tab, operation);
        operation.check();
        if (tab.view.webContents.isDevToolsOpened()) fail('control_denied');
        if (performance.now() >= leaseDeadline) fail('control_revoked');
        tab.lease = { deadline: leaseDeadline, expiresAt,
          timer: setTimeout(() => this.revoke(tab, 'control_revoked', true), Math.max(1, leaseDeadline - performance.now())) };
        return this.metadata(input.browser_id);
      }
      if (tab.navigating && input.method !== 'browser.wait' && !navigation) fail('stale_reference');
      const result = await this.execute(tab, operation, input.method, params, revision);
      operation.check();
      return bounded(result);
    } catch (error) {
      if (error instanceof Error && error.message === 'timeout') this.revoke(tab, 'timeout', true);
      throw this.safeError(error);
    } finally { this.finish(tab, operation); }
  }

  private async execute(tab: Tab, operation: BrowserOperation, method: string, params: Record<string, unknown>, revision: bigint): Promise<unknown> {
    switch (method) {
      case 'browser.cdp.connect': {
        if (tab.cdp) fail('control_denied');
        if (!this.onCdpMessage || !tab.debuggerReady) fail('control_denied');
        const generation = tab.generation;
        const check = (): void => {
          this.assertLease(tab, generation);
          if (tab.cdp !== relay || !tab.debuggerReady) fail('control_revoked');
        };
        const relay = new BrowserCdp({
          check,
          send: async (method, params, sessionId, timeout) => {
            check();
            if (tab.pending.size >= 40) fail('limit_exceeded');
            const command = this.operation(tab, false, timeout, check);
            tab.cdpPending.add(command);
            try {
              return method === 'Input.dispatchKeyEvent' || method === 'Input.dispatchMouseEvent'
                ? await this.input(tab, command, method, params, sessionId)
                : await this.cdp(tab, command, method, params, sessionId);
            } finally { tab.cdpPending.delete(command); this.finish(tab, command); }
          },
          cleanup: (method, params, sessionId) => this.cleanup(tab, method, params, sessionId),
          describe: () => ({ url: tab.view.webContents.getURL(), title: tab.view.webContents.getTitle(), userAgent: tab.view.webContents.getUserAgent() }),
          contexts: () => {
            check();
            if (tab.cdpContextOverflow) fail('limit_exceeded');
            return [...tab.cdpContexts.values()];
          },
          reserveOutput: bytes => {
            if (this.cdpOutputMessages >= MAX_CDP_EVENT_MESSAGES || this.cdpOutputBytes + bytes > MAX_CDP_BYTES) fail('limit_exceeded');
            this.cdpOutputMessages++; this.cdpOutputBytes += bytes;
          },
          releaseOutput: bytes => { this.cdpOutputMessages--; this.cdpOutputBytes -= bytes; },
          write: async raw => {
            check();
            await this.onCdpMessage!(tab.descriptor.id, generation.toString(), raw);
            check();
          },
          revoke: reason => { if (tab.cdp === relay && tab.generation === generation) this.revoke(tab, reason, true); },
        }, this.fileBudget);
        tab.cdp = relay; // Reserve before initialization so a concurrent connect cannot duplicate it.
        try { await operation.wait(relay.initialize(operation.remaining())); }
        catch (error) {
          if (tab.cdp === relay) { tab.cdp = undefined; await relay.dispose(); }
          throw error;
        }
        return { connected: true };
      }
      case 'browser.cdp.send':
        if (!tab.cdp) fail('control_denied');
        await tab.cdp.receive(params.chunk, Number(params.timeout_ms ?? 10000));
        return { queued: true };
      case 'browser.navigate':
      case 'browser.back':
      case 'browser.forward':
      case 'browser.reload': {
        let command = 'Page.navigate';
        let args: Record<string, unknown> = { url: params.url };
        if (method === 'browser.reload') { command = 'Page.reload'; args = {}; }
        if (method === 'browser.back' || method === 'browser.forward') {
          const history = await this.cdp(tab, operation, 'Page.getNavigationHistory');
          const entry = history.entries?.[history.currentIndex + (method === 'browser.back' ? -1 : 1)];
          if (!entry || !allowedUrl(entry.url)) fail('unsupported');
          command = 'Page.navigateToHistoryEntry'; args = { entryId: entry.id };
        }
        operation.check();
        if (tab.revision !== revision) fail('stale_reference');
        const response = await this.cdp(tab, operation, command, args);
        if (response.errorText) fail('display_failed');
        return this.metadata(tab.descriptor.id);
      }
      case 'browser.snapshot': return this.snapshot(tab, operation);
      case 'browser.act': return this.act(tab, operation, params.action);
      case 'browser.evaluate': {
        const frameId = await this.mainFrame(tab, operation);
        const context = [...tab.contexts.values()].find(context => context.frameId === frameId);
        if (!context) fail('stale_reference');
        const objectGroup = `luvus:${randomUUID()}`;
        try {
          const result = await this.cdp(tab, operation, 'Runtime.evaluate', {
            expression: params.expression, uniqueContextId: context.uniqueId, returnByValue: true, awaitPromise: true,
            timeout: operation.remaining(), objectGroup,
          });
          return { ...this.metadata(tab.descriptor.id), ...this.runtimeValue(result) };
        } finally { this.cleanup(tab, 'Runtime.releaseObjectGroup', { objectGroup }); }
      }
      case 'browser.screenshot': {
        operation.check();
        const image = await operation.wait(tab.view.webContents.capturePage());
        operation.check();
        const png = image.toPNG();
        const cap = Number(params.max_bytes ?? MAX_RESULT_BYTES);
        if (4 * Math.ceil(png.length / 3) > cap) fail('limit_exceeded');
        return bounded({ document_revision: tab.revision.toString(), mime_type: 'image/png', base64: png.toString('base64') }, cap);
      }
      case 'browser.wait': return this.waitForPage(tab, operation, record(params.condition));
      case 'browser.dialog':
        if (!tab.dialog) fail('unsupported');
        await this.cdp(tab, operation, 'Page.handleJavaScriptDialog', { accept: params.accept, ...(params.prompt_text !== undefined ? { promptText: params.prompt_text } : {}) });
        return this.metadata(tab.descriptor.id);
      default: fail('unsupported');
    }
  }

  private async snapshot(tab: Tab, operation: BrowserOperation): Promise<unknown> {
    const frameId = await this.mainFrame(tab, operation);
    const tree = await this.cdp(tab, operation, 'Accessibility.getFullAXTree', { frameId });
    const offered = new Map<string, number>();
    const nodes: Array<Record<string, unknown>> = [];
    let bytes = 0;
    let truncated = false;
    const raw: AXNode[] = Array.isArray(tree.nodes) ? tree.nodes : [];
    let scanned = 0;
    for (const node of raw) {
      if (++scanned > MAX_REFS * 8) { truncated = true; break; }
      if (node.ignored || !Number.isSafeInteger(node.backendDOMNodeId) || node.backendDOMNodeId! <= 0) continue;
      if (nodes.length >= MAX_REFS) { truncated = true; break; }
      const ref = `n${++tab.nextRef}`;
      const item = { node: ref, ax_id: shortText(node.nodeId, 128), role: shortText(node.role?.value, 128),
        name: shortText(node.name?.value), value: shortText(node.value?.value), description: shortText(node.description?.value),
        children: (node.childIds ?? []).slice(0, MAX_REFS).map(id => shortText(id, 128)) };
      bytes += Buffer.byteLength(JSON.stringify(item)) + 1;
      if (bytes > MAX_RESULT_BYTES - 32768) { truncated = true; break; }
      offered.set(ref, node.backendDOMNodeId!);
      nodes.push(item);
    }
    operation.check();
    const result = bounded({ ...this.metadata(tab.descriptor.id), nodes, truncated });
    tab.refs = offered;
    return result;
  }

  private async act(tab: Tab, operation: BrowserOperation, value: unknown): Promise<unknown> {
    const action = actionParams(value);
    if (action.document_revision !== tab.revision.toString()) fail('stale_reference');
    const backendNodeId = tab.refs.get(action.node);
    if (backendNodeId === undefined) fail('stale_reference');
    operation.fence(() => {
      if (tab.refs.get(action.node) !== backendNodeId || action.document_revision !== tab.revision.toString()) fail('stale_reference');
    });
    const { id: executionContextId } = await this.isolatedWorld(tab, operation);
    const objectGroup = `luvus:${randomUUID()}`;
    try {
      const resolved = await this.cdp(tab, operation, 'DOM.resolveNode', { backendNodeId, executionContextId, objectGroup });
      const objectId = resolved.object?.objectId;
      if (typeof objectId !== 'string') fail('stale_reference');
      const call = async (kind: string, first?: unknown, second?: unknown): Promise<void> => {
        const response = await this.cdp(tab, operation, 'Runtime.callFunctionOn', { objectId, functionDeclaration: NODE_ACTION,
          arguments: [{ value: kind }, { value: first ?? null }, { value: second ?? null }], returnByValue: true });
        if (this.runtimeValue(response).value !== true) fail('stale_reference');
      };
      await call('check');
      switch (action.kind) {
        case 'fill': await call('fill', action.text); break;
        case 'select': await call('select', action.values); break;
        case 'scroll': await call('scroll', action.x, action.y); break;
        case 'press':
          await call('focus');
          await this.input(tab, operation, 'Input.dispatchKeyEvent', { type: 'keyDown', ...action.key });
          await this.input(tab, operation, 'Input.dispatchKeyEvent', { type: 'keyUp', ...action.key, text: undefined });
          break;
        case 'click': {
          await this.cdp(tab, operation, 'DOM.scrollIntoViewIfNeeded', { objectId });
          await call('check');
          const box = await this.cdp(tab, operation, 'DOM.getBoxModel', { objectId });
          const quad = box.model?.content;
          if (!Array.isArray(quad) || quad.length !== 8 || !quad.every(Number.isFinite)) fail('stale_reference');
          const x = Math.round((quad[0] + quad[2] + quad[4] + quad[6]) / 4);
          const y = Math.round((quad[1] + quad[3] + quad[5] + quad[7]) / 4);
          const metrics = await this.cdp(tab, operation, 'Page.getLayoutMetrics');
          const viewport = metrics.cssVisualViewport;
          if (!viewport || x < 0 || y < 0 || x >= viewport.clientWidth || y >= viewport.clientHeight) fail('unsupported');
          const hit = await this.cdp(tab, operation, 'Runtime.callFunctionOn', { objectId,
            functionDeclaration: 'function(x,y) { return this.isConnected && this.ownerDocument === document && this.contains(document.elementFromPoint(x,y)); }',
            arguments: [{ value: x }, { value: y }], returnByValue: true });
          if (this.runtimeValue(hit).value !== true) fail('stale_reference');
          await this.input(tab, operation, 'Input.dispatchMouseEvent', { type: 'mousePressed', x, y, button: 'left', clickCount: 1 });
          await this.input(tab, operation, 'Input.dispatchMouseEvent', { type: 'mouseReleased', x, y, button: 'left', clickCount: 1 });
          break;
        }
      }
      return { document_revision: tab.revision.toString(), acted: true };
    } finally { this.cleanup(tab, 'Runtime.releaseObjectGroup', { objectGroup }); }
  }

  private async waitForPage(tab: Tab, operation: BrowserOperation, condition: Record<string, unknown>): Promise<unknown> {
    if (condition.load === true) {
      const contents = tab.view.webContents;
      let check: () => void = () => {};
      try {
        operation.check();
        await operation.wait(new Promise<void>(resolve => {
          check = () => { if (!contents.isDestroyed() && !contents.isLoadingMainFrame()) resolve(); };
          contents.on('did-stop-loading', check);
          contents.on('did-finish-load', check);
          check();
        }));
        return { ...this.metadata(tab.descriptor.id), matched: true };
      } finally {
        contents.removeListener('did-stop-loading', check);
        contents.removeListener('did-finish-load', check);
      }
    }
    const { uniqueId: uniqueContextId } = await this.isolatedWorld(tab, operation);
    const token = randomUUID();
    try {
      const result = await this.cdp(tab, operation, 'Runtime.callFunctionOn', { uniqueContextId,
        functionDeclaration: WAIT_FOR_PAGE, arguments: [{ value: token }, { value: condition }, { value: Math.max(1, operation.remaining() - 1) }],
        returnByValue: true, awaitPromise: true });
      if (this.runtimeValue(result).value !== true) fail('timeout');
      return { ...this.metadata(tab.descriptor.id), matched: true };
    } finally {
      // A unique context cannot address a replacement document, even across renderer processes.
      this.cleanup(tab, 'Runtime.callFunctionOn', { uniqueContextId,
        functionDeclaration: CANCEL_WAIT, arguments: [{ value: token }], returnByValue: true });
    }
  }

  private async input(tab: Tab, operation: BrowserOperation, method: string, params: Record<string, unknown>, sessionId?: string): Promise<Record<string, any>> {
    operation.check();
    if (method === 'Input.dispatchKeyEvent') {
      const modifiers = Number(params.modifiers ?? 0);
      const key = String(params.key).toLowerCase();
      if ((modifiers & 6) && ['c', 'v', 'x'].includes(key) || key === 'insert' && (modifiers & 10)) fail('unsupported');
    }
    const previous = tab.inputTail;
    let release: () => void = () => {};
    tab.inputTail = new Promise<void>(resolve => { release = resolve; });
    let expected: Tab['input'];
    try {
      await operation.wait(previous);
      const mouseTypes: Record<string, string> = { mousePressed: 'mouseDown', mouseReleased: 'mouseUp', mouseMoved: 'mouseMove', mouseWheel: 'mouseWheel' };
      const admission = { consumed: false, observed: false, wheelGestures: new Set<string>() };
      expected = method === 'Input.dispatchKeyEvent'
        ? { kind: 'key', type: params.type === 'keyUp' ? 'keyUp' : 'keyDown', key: String(params.key), modifiers: Number(params.modifiers ?? 0), ...admission }
        : { kind: 'mouse', type: mouseTypes[String(params.type)]!, x: Math.fround(Number(params.x)), y: Math.fround(Number(params.y)), ...admission };
      tab.input = expected;
      return await this.cdp(tab, operation, method, params, sessionId);
    } finally {
      if (expected && tab.input === expected) tab.input = undefined;
      release();
    }
  }

  private runtimeValue(reply: RuntimeReply): { value?: unknown; unserializable_value?: string } {
    if (reply.exceptionDetails) {
      const description = reply.exceptionDetails.exception?.description ?? '';
      if (description.startsWith('Error: stale_reference')) fail('stale_reference');
      if (description.startsWith('Error: unsupported')) fail('unsupported');
      fail('display_failed');
    }
    if (!reply.result) fail('display_failed');
    if (reply.result.unserializableValue !== undefined) return bounded({ unserializable_value: reply.result.unserializableValue });
    return bounded({ value: reply.result.value ?? null });
  }

  private async mainFrame(tab: Tab, operation: BrowserOperation): Promise<string> {
    const result = await this.cdp(tab, operation, 'Page.getFrameTree');
    const frame = result.frameTree?.frame;
    if (!frame || !allowedUrl(frame.url)) fail('stale_reference');
    return text(frame.id, 256);
  }

  private async isolatedWorld(tab: Tab, operation: BrowserOperation): Promise<Context> {
    const frameId = await this.mainFrame(tab, operation);
    const world = await this.cdp(tab, operation, 'Page.createIsolatedWorld', { frameId, worldName: 'luvus-browser-tools' });
    const id = integer(world.executionContextId, 1, Number.MAX_SAFE_INTEGER);
    if (tab.isolatedContext?.id !== id || tab.isolatedContext.frameId !== frameId) fail('stale_reference');
    return tab.isolatedContext;
  }

  private cacheCdpContext(tab: Tab, method: string, params: Record<string, any>): void {
    if (method === 'Runtime.executionContextsCleared') {
      tab.cdpContexts.clear(); tab.cdpContextOverflow = false;
    } else if (method === 'Runtime.executionContextDestroyed') tab.cdpContexts.delete(params.executionContextId);
    else if (method === 'Runtime.executionContextCreated') {
      try {
        const context = record(params.context);
        const id = integer(context.id, 1, Number.MAX_SAFE_INTEGER);
        if (!tab.cdpContexts.has(id) && tab.cdpContexts.size >= 256) fail('limit_exceeded');
        // Runtime is already enabled by the host. A CDP client enabling it later needs the
        // current context inventory, since Chromium need not replay an idempotent enable.
        tab.cdpContexts.set(id, structuredClone(bounded(context, 8192)));
      } catch {
        tab.cdpContextOverflow = true;
        if (tab.cdp) this.revoke(tab, 'limit_exceeded', true);
      }
    }
  }

  private async attach(tab: Tab, operation: BrowserOperation): Promise<void> {
    operation.check();
    if (tab.view.webContents.isDevToolsOpened()) fail('control_denied');
    if (!tab.view.webContents.debugger.isAttached()) {
      tab.debuggerReady = false;
      tab.view.webContents.debugger.attach('1.3');
    }
    if (!tab.debuggerReady) {
      await this.cdp(tab, operation, 'Page.enable');
      const tree = await this.cdp(tab, operation, 'Page.getFrameTree');
      tab.frameId = text(tree.frameTree?.frame?.id, 256);
      await this.cdp(tab, operation, 'Runtime.enable');
      await this.cdp(tab, operation, 'DOM.enable');
      // Page clicks cannot launch an unrestricted display file chooser.
      await this.cdp(tab, operation, 'Page.setInterceptFileChooserDialog', { enabled: true });
      tab.debuggerReady = true;
    }
  }

  // High-level tools and the scoped relay share this debugger and the same lifetime fences.
  private async cdp(tab: Tab, operation: BrowserOperation, method: string, params: Record<string, unknown> = {}, sessionId?: string): Promise<Record<string, any>> {
    operation.check();
    if (!tab.view.webContents.debugger.isAttached()) fail('control_revoked');
    return operation.wait(tab.view.webContents.debugger.sendCommand(method, params, sessionId));
  }

  private cleanup(tab: Tab, method: string, params: Record<string, unknown> = {}, sessionId?: string): void {
    if (!tab.view.webContents.isDestroyed() && tab.view.webContents.debugger.isAttached()) {
      void tab.view.webContents.debugger.sendCommand(method, params, sessionId).catch(() => {});
    }
  }

  private operation(tab: Tab, documentFenced: boolean, timeout: number, validate: () => void): BrowserOperation {
    const operation = new BrowserOperation(documentFenced, timeout, validate, () => this.revoke(tab, 'timeout', true));
    tab.pending.add(operation);
    return operation;
  }
  private finish(tab: Tab, operation: BrowserOperation): void { operation.finish(); tab.pending.delete(operation); }
  private alive(tab: Tab): void {
    if (this.tabs.get(tab.descriptor.id) !== tab || tab.view.webContents.isDestroyed()) fail('browser_closed');
  }
  private assertLease(tab: Tab, expected: bigint): void {
    this.alive(tab);
    if (!this.controlled(tab) || tab.generation !== expected) fail('control_revoked');
  }
  private controlled(tab: Tab): boolean {
    if (tab.lease && performance.now() >= tab.lease.deadline) this.revoke(tab, 'control_revoked', true);
    return tab.lease !== undefined;
  }
  private humanVisible(tab:Tab){return this.tabs.get(tab.descriptor.id)===tab && this.active===tab.descriptor.id && tab.ready && !tab.navigating && tab.view.getVisible();}
  private noteHumanGesture(tab:Tab){
    if(this.humanVisible(tab))tab.humanGesture={revision:tab.revision,generation:tab.generation,until:performance.now()+1800};
  }
  private invalidateDocument(tab: Tab): void {
    tab.humanGesture=undefined;
    tab.revision++;
    tab.refs.clear();
    tab.dialog = null;
    for (const operation of tab.pending) if (operation.documentFenced) operation.abort('stale_reference');
  }
  private revoke(tab: Tab, reason: string, notify: boolean, evenUncontrolled = false): void {
    const hadAuthority = tab.lease !== undefined || [...tab.pending].some(operation => !operation.controller.signal.aborted);
    const notifyOwner = notify && this.tabs.get(tab.descriptor.id) === tab && (hadAuthority || evenUncontrolled);
    const retiredGeneration=tab.generation.toString();
    // Fence an acquire already in transit when the human takes over. This high-water advance
    // only denies authority; the owner must send a strictly newer explicit acquisition to grant it.
    if (notifyOwner) tab.generation++;
    if (tab.lease) clearTimeout(tab.lease.timer);
    tab.lease = undefined;
    const relay = tab.cdp;
    tab.cdp = undefined;
    if (relay) {
      const cleanup = Promise.all([tab.retiringFiles,relay.dispose()]).then(()=>{});
      tab.retiringFiles=cleanup;
      this.cleaning.add(cleanup);
      const finished=()=>{this.cleaning.delete(cleanup);if(tab.retiringFiles===cleanup)tab.retiringFiles=undefined;};
      void cleanup.then(finished,finished);
    }
    tab.refs.clear();
    tab.input = undefined;
    for (const operation of tab.pending) operation.abort(reason);
    if (hadAuthority) this.cleanup(tab, 'Runtime.terminateExecution');
    if (notifyOwner) this.onControlRevoked?.(tab.descriptor.id, reason,retiredGeneration);
  }
  private safeError(error: unknown): Error {
    const code = error instanceof Error ? error.message : '';
    return new Error(['invalid_params', 'timeout', 'stale_reference', 'route_changed', 'browser_closed', 'control_revoked', 'control_denied', 'unsupported', 'limit_exceeded'].includes(code) ? code : 'display_failed');
  }

  bounds(id: string, bounds: Electron.Rectangle | null): void {
    const tab = this.tabs.get(id);
    if (!tab) return;
    if (!bounds) { tab.view.setVisible(false); return; }
    const { width, height } = this.window.getContentBounds();
    if (![bounds.x, bounds.y, bounds.width, bounds.height].every(Number.isFinite) || bounds.x < 0 || bounds.y < 0 || bounds.width < 0 || bounds.height < 0 || bounds.x + bounds.width > width + 1 || bounds.y + bounds.height > height + 1) throw new Error('Invalid browser bounds');
    this.show(id);
    tab.view.setBounds({ x: Math.round(bounds.x), y: Math.round(bounds.y), width: Math.floor(bounds.width), height: Math.floor(bounds.height) });
    tab.view.setVisible(bounds.width > 0 && bounds.height > 0);
  }

  private show(id: string): void {
    for (const [key, tab] of this.tabs) if (key !== id) tab.view.setVisible(false);
    this.active = id;
  }

  close(id: string): Promise<void> {
    const pending = this.closing.get(id);
    if (pending) return pending;
    const tab = this.tabs.get(id);
    if (!tab) return Promise.resolve();
    this.tabs.delete(id);
    this.retired.add(id);
    tab.humanDownload?.cancel();
    this.revoke(tab, 'browser_closed', false);
    if (!this.window.isDestroyed()) this.window.contentView.removeChildView(tab.view);
    const browserSession = tab.browserSession;
    if (!tab.view.webContents.isDestroyed()) tab.view.webContents.close({ waitForBeforeUnload: false });
    if (this.active === id) this.active = undefined;
    const closing = Promise.resolve().then(async () => {
      const operation = new BrowserOperation(false, MAX_TIMEOUT_MS, () => {}, () => {});
      try {
        await operation.wait(browserSession.closeAllConnections());
        await operation.wait(browserSession.clearStorageData());
      } catch { /* Closing authority must not depend on storage cleanup. */ }
      finally { operation.finish(); this.closing.delete(id); }
    });
    this.closing.set(id, closing);
    return closing;
  }

  async dispose(): Promise<void> {
    await Promise.all([...this.tabs.keys()].map(id => this.close(id)));
    await Promise.all(this.closing.values());
    await Promise.all(this.cleaning);
  }
}
