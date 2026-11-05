import { fail } from './browser-policy.js';

/** A bounded operation, not a second broker. Every awaited boundary rechecks its owner fences. */
export class BrowserOperation {
  readonly controller = new AbortController();
  readonly deadline: number;
  private readonly timer: ReturnType<typeof setTimeout>;
  private readonly fences: Array<() => void> = [];

  constructor(readonly documentFenced: boolean, timeout: number, private readonly validate: () => void, onTimeout: () => void) {
    this.deadline = performance.now() + timeout;
    this.timer = setTimeout(() => { this.abort('timeout'); onTimeout(); }, timeout);
  }

  check(): void {
    if (this.controller.signal.aborted) throw this.controller.signal.reason;
    if (performance.now() >= this.deadline) fail('timeout');
    this.validate();
    for (const fence of this.fences) fence();
  }

  fence(validate: () => void): void { this.fences.push(validate); this.check(); }
  abort(reason: string): void { this.controller.abort(new Error(reason)); }
  finish(): void { clearTimeout(this.timer); }
  remaining(): number { this.check(); return Math.max(1, Math.ceil(this.deadline - performance.now())); }

  async wait<T>(promise: Promise<T>): Promise<T> {
    // Always observe the promise, including when a synchronous event revoked us during dispatch.
    const signal = this.controller.signal;
    let onAbort: () => void = () => {};
    try {
      const value = await Promise.race([promise, new Promise<never>((_resolve, reject) => {
        onAbort = () => reject(signal.reason);
        signal.addEventListener('abort', onAbort, { once: true });
        if (signal.aborted) onAbort();
      })]);
      this.check();
      return value;
    } finally { signal.removeEventListener('abort', onAbort); }
  }
}

// Static functions execute in an isolated world on the current main document. All data is passed
// as CDP argument values, never spliced into JavaScript source. No bridge or Node globals exist.
export const NODE_ACTION = `function(kind, value, other) {
  if (!this.isConnected || this.ownerDocument !== document) throw new Error('stale_reference');
  if (kind === 'check') return true;
  if (kind === 'focus') { this.focus(); return document.activeElement === this; }
  if (kind === 'fill') {
    let prototype;
    if (this instanceof HTMLTextAreaElement) prototype = HTMLTextAreaElement.prototype;
    else if (this instanceof HTMLInputElement && !['file','checkbox','radio','button','submit','reset','image','hidden'].includes(this.type)) prototype = HTMLInputElement.prototype;
    else if (this.isContentEditable) { this.focus(); this.textContent = value; }
    else throw new Error('unsupported');
    if (this.disabled || this.readOnly) throw new Error('unsupported');
    if (prototype) { this.focus(); Object.getOwnPropertyDescriptor(prototype, 'value').set.call(this, value); }
    this.dispatchEvent(new Event('input', { bubbles: true }));
    this.dispatchEvent(new Event('change', { bubbles: true }));
    return true;
  }
  if (kind === 'select') {
    if (!(this instanceof HTMLSelectElement) || this.disabled || (!this.multiple && value.length > 1)) throw new Error('unsupported');
    if (value.some(item => !Array.from(this.options).some(option => option.value === item && !option.disabled))) throw new Error('unsupported');
    for (const option of this.options) option.selected = value.includes(option.value);
    this.dispatchEvent(new Event('input', { bubbles: true }));
    this.dispatchEvent(new Event('change', { bubbles: true }));
    return true;
  }
  if (kind === 'scroll') { this.scrollBy({ left: value, top: other, behavior: 'instant' }); return true; }
  throw new Error('unsupported');
}`;

export const WAIT_FOR_PAGE = `function(token, condition, timeout) {
  const pending = globalThis.__luvusBrowserWaits ??= new Map();
  return new Promise(resolve => {
    let observer;
    let timer;
    const finish = matched => {
      observer?.disconnect();
      clearTimeout(timer);
      removeEventListener('load', check);
      removeEventListener('pagehide', cancel);
      pending.delete(token);
      resolve(matched);
    };
    const check = () => {
      if (condition.load === true ? document.readyState === 'complete' : (document.body?.textContent ?? '').includes(condition.text)) finish(true);
    };
    const cancel = () => finish(false);
    pending.set(token, cancel);
    addEventListener('pagehide', cancel, { once: true });
    if (condition.load === true) addEventListener('load', check);
    else { observer = new MutationObserver(check); observer.observe(document, { subtree: true, childList: true, characterData: true }); }
    timer = setTimeout(cancel, timeout);
    check();
  });
}`;

export const CANCEL_WAIT = `function(token) { globalThis.__luvusBrowserWaits?.get(token)?.(); }`;
