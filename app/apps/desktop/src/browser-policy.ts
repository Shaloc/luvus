import type { Target } from '@luvus/app-protocol';

// Older transport-21 owners require an HTTP(S) URL even for a new tab.
export const LEGACY_BLANK_TAB = 'https://new-tab.luvus.invalid/';
export const displayBrowserUrl = (url: string) => url === LEGACY_BLANK_TAB ? 'about:blank' : url;

export const MAX_RESULT_BYTES = 1_044_480;
export const MAX_TIMEOUT_MS = 20_000;
export const MAX_REFS = 512;
export const MAX_CDP_BYTES = 64 * 1024 * 1024;
export const MAX_CDP_MESSAGES = 32;
export const MAX_CDP_EVENT_MESSAGES = 128;
export const MAX_CDP_CHUNK_BYTES = 8192;
export const MAX_CDP_EVENT_CHUNK_BYTES = 128 * 1024;

export function rendererMayRequest(method: string): boolean {
  return !method.startsWith('browser.') || ['browser.open', 'browser.panel', 'browser.list', 'browser.get', 'browser.control.status'].includes(method);
}

export interface CdpChunk { transfer: string; index: number; total: number; text: string }

export function cdpChunk(value: unknown): CdpChunk {
  const chunk = record(value);
  fields(chunk, ['transfer', 'index', 'total', 'text']);
  const transfer = text(chunk.transfer, 32);
  if (!/^[0-9a-f]{32}$/.test(transfer)) fail('invalid_params');
  const total = integer(chunk.total, 1, 16384);
  const index = integer(chunk.index, 0, total - 1);
  const content = text(chunk.text, MAX_CDP_CHUNK_BYTES);
  // Each transport chunk must itself be UTF-8, not half a surrogate pair.
  if (Buffer.from(content, 'utf8').toString('utf8') !== content) fail('invalid_params');
  return { transfer, index, total, text: content };
}

// Reviewed, target-local CDP surface. No wildcard domains, streams, disk paths, debugger
// globals, TLS overrides, permissions, new targets, or context-wide Storage operations.
export const CDP_PAGE_COMMANDS = new Set([
  'Page.enable', 'Page.disable', 'Page.getFrameTree', 'Page.getLayoutMetrics',
  'Page.navigate', 'Page.reload', 'Page.stopLoading', 'Page.getNavigationHistory', 'Page.navigateToHistoryEntry',
  'Page.setLifecycleEventsEnabled', 'Page.addScriptToEvaluateOnNewDocument', 'Page.removeScriptToEvaluateOnNewDocument',
  'Page.createIsolatedWorld', 'Page.handleJavaScriptDialog', 'Page.captureScreenshot',
  'Page.setInterceptFileChooserDialog', 'Page.bringToFront',
  'DOM.enable', 'DOM.disable', 'DOM.getDocument', 'DOM.getFlattenedDocument', 'DOM.describeNode',
  'DOM.resolveNode', 'DOM.requestNode', 'DOM.requestChildNodes', 'DOM.querySelector', 'DOM.querySelectorAll',
  'DOM.getBoxModel', 'DOM.getContentQuads', 'DOM.getNodeForLocation', 'DOM.getFrameOwner',
  'DOM.getAttributes', 'DOM.getOuterHTML', 'DOM.focus', 'DOM.scrollIntoViewIfNeeded',
  'DOM.setNodeValue', 'DOM.setAttributeValue', 'DOM.removeAttribute', 'DOM.setOuterHTML',
  'DOM.performSearch', 'DOM.getSearchResults', 'DOM.discardSearchResults',
  'Accessibility.enable', 'Accessibility.disable', 'Accessibility.getFullAXTree',
  'Accessibility.getPartialAXTree', 'Accessibility.getRootAXNode', 'Accessibility.getChildAXNodes', 'Accessibility.queryAXTree',
  'Runtime.enable', 'Runtime.disable', 'Runtime.evaluate', 'Runtime.callFunctionOn', 'Runtime.awaitPromise',
  'Runtime.getProperties', 'Runtime.releaseObject', 'Runtime.releaseObjectGroup', 'Runtime.queryObjects',
  'Runtime.addBinding', 'Runtime.removeBinding', 'Runtime.runIfWaitingForDebugger',
  'Input.dispatchKeyEvent', 'Input.dispatchMouseEvent', 'Input.insertText',
  'Network.enable', 'Network.disable', 'Network.getResponseBody', 'Network.getRequestPostData',
  'Network.setCacheDisabled', 'Network.setBypassServiceWorker', 'Network.setExtraHTTPHeaders',
  'Network.setUserAgentOverride', 'Network.emulateNetworkConditions', 'Network.setBlockedURLs',
  'Network.getCookies', 'Network.setCookie', 'Network.setCookies', 'Network.deleteCookies',
  'Emulation.setDeviceMetricsOverride', 'Emulation.clearDeviceMetricsOverride', 'Emulation.setVisibleSize',
  'Emulation.setEmulatedMedia', 'Emulation.setUserAgentOverride', 'Emulation.setGeolocationOverride',
  'Emulation.clearGeolocationOverride', 'Emulation.setTimezoneOverride', 'Emulation.setLocaleOverride',
  'Emulation.setDefaultBackgroundColorOverride', 'Emulation.setScriptExecutionDisabled',
  'Emulation.setTouchEmulationEnabled', 'Emulation.setFocusEmulationEnabled', 'Emulation.setScrollbarsHidden',
  'Emulation.setIdleOverride', 'Emulation.clearIdleOverride',
  'Fetch.enable', 'Fetch.disable', 'Fetch.continueRequest', 'Fetch.continueResponse', 'Fetch.fulfillRequest',
  'Fetch.failRequest', 'Fetch.continueWithAuth', 'Fetch.getResponseBody',
  'Log.enable', 'Log.disable', 'Log.clear', 'Performance.enable', 'Performance.disable', 'Performance.getMetrics',
]);

export function cdpPageParams(method: string, params: Record<string, unknown>): void {
  if (!CDP_PAGE_COMMANDS.has(method)) fail('unsupported');
  for (const key of ['browserContextId', 'targetId', 'sessionId', 'downloadPath', 'filePath', 'files', 'transferMode']) {
    if (params[key] !== undefined) fail('unsupported');
  }
  if (method === 'Page.navigate' && !allowedUrl(params.url)) fail('unsupported');
  if (method === 'Fetch.continueRequest' && params.url !== undefined && !allowedUrl(params.url)) fail('unsupported');
  if (method === 'Page.setInterceptFileChooserDialog' && params.enabled !== true) fail('unsupported');
  if (method !== 'Page.createIsolatedWorld' && (params.grantUniveralAccess === true || params.grantUniversalAccess === true)) fail('unsupported');
  if (params.worldName === 'luvus-browser-tools' || params.executionContextName === 'luvus-browser-tools'
      || typeof params.objectGroup === 'string' && params.objectGroup.startsWith('luvus:')) fail('unsupported');
  if (method === 'Input.dispatchKeyEvent') {
    if (!['keyDown', 'rawKeyDown', 'keyUp', 'char'].includes(String(params.type))) fail('invalid_params');
    text(params.key, 256);
    integer(params.modifiers ?? 0, 0, 15);
    // Playwright sends an empty list on ordinary keys and selectAll on macOS. Do not
    // expose platform clipboard or other native editor actions through this parameter.
    if (params.commands !== undefined && (!Array.isArray(params.commands) || params.commands.length > 1
        || params.commands.some(command => command !== 'selectAll'))) fail('unsupported');
  }
  if (method === 'Input.dispatchMouseEvent') {
    if (!['mousePressed', 'mouseReleased', 'mouseMoved', 'mouseWheel'].includes(String(params.type))) fail('invalid_params');
    for (const key of ['x', 'y']) if (typeof params[key] !== 'number' || !Number.isFinite(params[key])) fail('invalid_params');
    if (params.type === 'mouseWheel') {
      for (const key of ['deltaX', 'deltaY']) if (typeof params[key] !== 'number' || !Number.isFinite(params[key])) fail('invalid_params');
    }
  }
}

export function fail(code: string): never { throw new Error(code); }

export function record(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) fail('invalid_params');
  return value as Record<string, unknown>;
}

export function fields(value: Record<string, unknown>, allowed: string[]): void {
  if (Object.keys(value).some(key => !allowed.includes(key))) fail('invalid_params');
}

export function text(value: unknown, max: number, empty = false): string {
  if (typeof value !== 'string' || (!empty && !value.length) || Buffer.byteLength(value) > max) fail('invalid_params');
  return value;
}

export function integer(value: unknown, min: number, max: number): number {
  if (typeof value !== 'number' || !Number.isInteger(value) || value < min || value > max) fail('invalid_params');
  return value;
}

export function generation(value: unknown): bigint {
  if (typeof value !== 'string' || !/^(0|[1-9][0-9]{0,19})$/.test(value)) fail('invalid_params');
  return BigInt(value);
}

export function sameTarget(a: Target, b: Target): boolean {
  return a.generation === b.generation && a.workspace_id === b.workspace_id && a.tab_id === b.tab_id
    && a.pane_id === b.pane_id && a.terminal_id === b.terminal_id;
}

export function validateTarget(value: Target): void {
  const target = record(value);
  fields(target, ['generation', 'workspace_id', 'tab_id', 'pane_id', 'terminal_id']);
  for (const key of ['generation', 'workspace_id', 'tab_id', 'pane_id']) text(target[key], 128);
  if (target.terminal_id !== null) text(target.terminal_id, 128);
}

export function allowedUrl(value: unknown): value is string {
  if (typeof value !== 'string' || Buffer.byteLength(value) > 8192) return false;
  if (value === 'about:blank') return true;
  if (!/^https?:\/\/[^/\s][^\s]*$/.test(value)) return false;
  try { return ['http:', 'https:'].includes(new URL(value).protocol); }
  catch { return false; }
}

export function humanBrowserAddress(value: string): string {
  const address = value.trim() || 'about:blank';
  if (allowedUrl(address)) return address;
  // Only infer schemes for hostnames. Never reinterpret a file, script or other scheme.
  if (!/^[a-z\d.-]+(?::\d+)?(?:[/?#]|$)/i.test(address) && !/^\[[\da-f:]+\](?::\d+)?(?:[/?#]|$)/i.test(address)) fail('invalid_params');
  const loopback = /^(localhost|127(?:\.\d+){3}|\[::1\])(?=[:/?#]|$)/i.test(address);
  const url = (loopback ? 'http://' : 'https://') + address;
  if (!allowedUrl(url)) fail('invalid_params');
  return new URL(url).href;
}

export function bounded<T>(value: T, max = MAX_RESULT_BYTES): T {
  let encoded: string | undefined;
  try { encoded = JSON.stringify(value); } catch { fail('limit_exceeded'); }
  if (encoded === undefined || Buffer.byteLength(encoded) > max) fail('limit_exceeded');
  return value;
}

// Page strings are untrusted; cap each field before constructing a snapshot.
export function shortText(value: unknown, max = 2048): string {
  return typeof value === 'string' ? Buffer.from(value.slice(0, max)).subarray(0, max).toString('utf8') : '';
}

export interface KeyStroke {
  key: string;
  code: string;
  windowsVirtualKeyCode: number;
  modifiers: number;
  text?: string;
}

export function keyStroke(value: unknown): KeyStroke {
  const parts = text(value, 128).split('+');
  const key = parts.pop()!;
  let modifiers = 0;
  for (const part of parts) {
    const bit = ({ Alt: 1, Control: 2, Ctrl: 2, Meta: 4, Shift: 8 } as Record<string, number>)[part];
    if (!bit || modifiers & bit) fail('invalid_params');
    modifiers |= bit;
  }
  const named: Record<string, [string, number]> = {
    Enter: ['Enter', 13], Tab: ['Tab', 9], Escape: ['Escape', 27], Backspace: ['Backspace', 8],
    Delete: ['Delete', 46], ArrowLeft: ['ArrowLeft', 37], ArrowUp: ['ArrowUp', 38],
    ArrowRight: ['ArrowRight', 39], ArrowDown: ['ArrowDown', 40], Home: ['Home', 36],
    End: ['End', 35], PageUp: ['PageUp', 33], PageDown: ['PageDown', 34], Space: ['Space', 32],
  };
  const entry = named[key];
  if (entry) return { key: key === 'Space' ? ' ' : key, code: entry[0], windowsVirtualKeyCode: entry[1], modifiers,
    ...((key === 'Space' || key === 'Enter') && !(modifiers & 7) ? { text: key === 'Space' ? ' ' : '\r' } : {}) };
  if (!/^[a-zA-Z0-9]$/.test(key)) fail('invalid_params');
  const actual = modifiers & 8 ? key.toUpperCase() : key;
  return { key: actual, code: /^[0-9]$/.test(key) ? `Digit${key}` : `Key${key.toUpperCase()}`,
    windowsVirtualKeyCode: key.toUpperCase().charCodeAt(0), modifiers,
    ...(!(modifiers & 7) ? { text: actual } : {}) };
}

export type Action =
  | { kind: 'click'; node: string; document_revision: string }
  | { kind: 'fill'; node: string; document_revision: string; text: string }
  | { kind: 'select'; node: string; document_revision: string; values: string[] }
  | { kind: 'press'; node: string; document_revision: string; key: KeyStroke }
  | { kind: 'scroll'; node: string; document_revision: string; x: number; y: number };

export function actionParams(value: unknown): Action {
  const action = record(value);
  const base = { node: text(action.node, 128), document_revision: text(action.document_revision, 128) };
  const common = ['kind', 'node', 'document_revision'];
  switch (action.kind) {
    case 'click': fields(action, common); return { ...base, kind: 'click' };
    case 'fill': fields(action, [...common, 'text']); return { ...base, kind: 'fill', text: text(action.text, 32768, true) };
    case 'select': {
      fields(action, [...common, 'values']);
      if (!Array.isArray(action.values) || action.values.length > 100) fail('invalid_params');
      return { ...base, kind: 'select', values: action.values.map(value => text(value, 1024, true)) };
    }
    case 'press': fields(action, [...common, 'key']); return { ...base, kind: 'press', key: keyStroke(action.key) };
    case 'scroll': fields(action, [...common, 'x', 'y']); return { ...base, kind: 'scroll', x: integer(action.x, -100000, 100000), y: integer(action.y, -100000, 100000) };
    default: fail('invalid_params');
  }
}

export function commandParams(method: string, value: unknown): Record<string, unknown> {
  const params = record(value);
  bounded(params, 65536);
  integer(params.timeout_ms ?? 10000, 1, MAX_TIMEOUT_MS);
  const extras: Record<string, string[]> = {
    'browser.control.acquire': ['ttl_s'], 'browser.control.release': [],
    'browser.navigate': ['url'], 'browser.back': [], 'browser.forward': [], 'browser.reload': [],
    'browser.close': [], 'browser.snapshot': [], 'browser.act': ['action'],
    'browser.evaluate': ['expression'], 'browser.screenshot': ['max_bytes'],
    'browser.wait': ['condition'], 'browser.dialog': ['accept', 'prompt_text'],
    'browser.cdp.connect': [], 'browser.cdp.send': ['chunk'],
  };
  const allowed = extras[method];
  if (!allowed) fail('unsupported');
  fields(params, ['timeout_ms', ...allowed]);
  switch (method) {
    case 'browser.cdp.send': cdpChunk(params.chunk); break;
    case 'browser.control.acquire': integer(params.ttl_s ?? 300, 1, 3600); break;
    case 'browser.navigate': if (!allowedUrl(params.url)) fail('invalid_params'); break;
    case 'browser.act': actionParams(params.action); break;
    case 'browser.evaluate': text(params.expression, 32768); break;
    case 'browser.screenshot': integer(params.max_bytes ?? MAX_RESULT_BYTES, 1, MAX_RESULT_BYTES); break;
    case 'browser.wait': {
      const condition = record(params.condition);
      if (condition.load === true) fields(condition, ['load']);
      else { fields(condition, ['text']); text(condition.text, 4096); }
      break;
    }
    case 'browser.dialog':
      if (typeof params.accept !== 'boolean') fail('invalid_params');
      if (params.prompt_text !== undefined) text(params.prompt_text, 4096, true);
      break;
  }
  return params;
}
