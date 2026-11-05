import type { Request, Target, TerminalFrame } from '@luvus/app-protocol';
import { safeLink, unpackColor } from './colors.js';
import { accessibleText, cellAt, cellPoint, keyRoute, selectionRange, styledRuns, takeInputBatch, terminalButton, type Point, type SelectionMode } from './model.js';
export { safeLink, unpackColor } from './colors.js';

type Input = Extract<Request, { type: 'input' }>['input'];
export interface TerminalOptions {
  target: Target;
  send: (request: Request) => Promise<unknown>;
  onError: (error: unknown) => void;
  onLink: (url: string) => void;
  onFocus: () => void;
  onMenu?: (x: number, y: number) => void;
  fontSize?: number;
  fontFamily?: string;
  /** Retained history is enabled by default; false disables host history gestures. */
  enableScroll?: boolean;
}

/** A cell painter and DOM input adapter. The core remains the only VT engine. */
export class CanvasTerminal {
  private readonly canvas = document.createElement('canvas');
  private readonly textarea = document.createElement('textarea');
  private readonly accessible = document.createElement('pre');
  private readonly waiting = document.createElement('span');
  private readonly preedit = document.createElement('span');
  private readonly abort = new AbortController();
  private readonly observer: ResizeObserver;
  private frame: TerminalFrame | null = null;
  private raf = 0;
  private resizeRaf = 0;
  private composing = false;
  private compositionCommit: string | null = null;
  private held = new Map<string, Extract<Input, { kind: 'key' }>>();
  private pendingTextKey: { code: string; shift: boolean; repeat: boolean } | null = null;
  private selection: { anchor: Point; cursor: Point; mode: SelectionMode; revision: string } | null = null;
  private pointer: { id: number; button: number; reporting: boolean; geometry: string } | null = null;
  private click = { time: 0, row: -1, col: -1, count: 0 };
  private wheelPoint: Point = [0, 0];
  private viewport = '';
  private width = 0;
  private height = 0;
  private cellWidth = 8;
  private cellHeight = 18;
  private foreground = '#f1f3f7';
  private background = '#000000';
  private disposed = false;
  private readonly images:TerminalImages;
  private queue: Request[] = [];
  private sending = false;
  private wheelLines = 0;
  private wheelRaf = 0;
  private fontSize: number;
  private fontFamily: string;
  private backgroundOpacity = 1;
  private rightClickToApp = false;

  setAppearance(value: { fontSize: number; fontFamily: string; backgroundOpacity: number; rightClickToApp: boolean; foreground: string; background: string }) {
    this.fontSize = Math.min(32, Math.max(9, value.fontSize));
    this.fontFamily = value.fontFamily;
    this.backgroundOpacity = Math.min(1, Math.max(0, value.backgroundOpacity));
    this.rightClickToApp = value.rightClickToApp;
    this.host.style.setProperty('--terminal-fg', value.foreground);
    this.host.style.setProperty('--terminal-bg', value.background);
    this.preedit.style.font = this.textarea.style.font = `${this.fontSize}px ${this.fontFamily}`;
    this.measure(); this.schedule();
  }

  constructor(private readonly host: HTMLElement, private readonly options: TerminalOptions) {
    this.images=new TerminalImages(options.target,options.send,()=>this.schedule());
    this.fontSize = Math.min(32, Math.max(9, options.fontSize ?? 13));
    this.fontFamily = options.fontFamily ?? '"SFMono-Regular", Menlo, Monaco, Consolas, monospace';
    host.classList.add('terminal-surface');
    this.canvas.setAttribute('aria-hidden', 'true');
    this.textarea.className = 'terminal-input';
    this.textarea.setAttribute('aria-label', 'Terminal input');
    this.textarea.autocapitalize = 'off';
    this.textarea.autocomplete = 'off';
    this.textarea.spellcheck = false;
    this.accessible.className = 'terminal-accessible';
    this.accessible.setAttribute('aria-label', 'Visible terminal output');
    this.accessible.setAttribute('role', 'log');
    // Do not announce every output frame while an agent streams.
    this.accessible.setAttribute('aria-live', 'off');
    this.waiting.className = 'terminal-waiting';
    this.waiting.textContent = 'Waiting for core terminal cells…';
    this.waiting.setAttribute('role', 'status');
    this.preedit.className = 'terminal-preedit';
    this.preedit.setAttribute('aria-hidden', 'true');
    this.preedit.style.font = `${this.fontSize}px ${this.fontFamily}`;
    this.textarea.style.font = `${this.fontSize}px ${this.fontFamily}`;
    host.append(this.canvas, this.accessible, this.textarea, this.waiting, this.preedit);
    const signal = this.abort.signal;
    this.textarea.addEventListener('keydown', this.keydown, { signal });
    this.textarea.addEventListener('keyup', this.keyup, { signal });
    this.textarea.addEventListener('beforeinput', this.beforeinput, { signal });
    this.textarea.addEventListener('compositionstart', () => { this.composing = true; this.compositionCommit = null; this.pendingTextKey = null; }, { signal });
    this.textarea.addEventListener('compositionupdate', (event) => { this.preedit.textContent = event.data; }, { signal });
    this.textarea.addEventListener('compositionend', (event) => {
      this.composing = false;
      this.preedit.textContent = '';
      if (event.data) this.input({ kind: 'paste', text: event.data });
      this.compositionCommit = event.data;
      this.textarea.value = '';
    }, { signal });
    this.textarea.addEventListener('input', () => { if (!this.composing) this.textarea.value = ''; }, { signal });
    this.textarea.addEventListener('paste', (event) => {
      event.preventDefault();
      const text = event.clipboardData?.getData('text/plain');
      if (text) this.input({ kind: 'paste', text });
    }, { signal });
    this.textarea.addEventListener('focus', () => {
      options.onFocus();
      if (this.frame) this.input({ kind: 'focus_event', focused: true });
      this.schedule();
    }, { signal });
    this.textarea.addEventListener('blur', () => {
      for (const key of this.held.values()) this.input({ ...key, repeat: false, release: true });
      this.held.clear(); this.pendingTextKey = null;
      if (this.frame) this.input({ kind: 'focus_event', focused: false });
      this.schedule();
    }, { signal });
    this.textarea.addEventListener('copy', event => { event.preventDefault(); this.copy(); }, { signal });
    this.canvas.addEventListener('pointerdown', this.pointerdown, { signal });
    this.canvas.addEventListener('pointermove', this.pointermove, { signal });
    this.canvas.addEventListener('pointerup', this.pointerup, { signal });
    this.canvas.addEventListener('pointercancel', this.pointerup, { signal });
    this.canvas.addEventListener('contextmenu', event => {
      event.preventDefault();
      if (this.rightClickToApp && this.frame?.modes.mouse && !this.frame.scroll_offset && !event.shiftKey) return;
      options.onMenu?.(event.clientX, event.clientY);
    }, { signal });
    host.addEventListener('wheel', (event) => {
      if (!this.frame || options.enableScroll === false) return;
      event.preventDefault();
      this.wheelPoint = this.point(event);
      const scale = event.deltaMode === 1 ? 1 : event.deltaMode === 2 ? this.frame.rows : 1 / this.cellHeight;
      this.wheelLines += event.deltaY * scale;
      if (!this.wheelRaf) this.wheelRaf = requestAnimationFrame(() => {
        this.wheelRaf = 0;
        const frame = this.frame;
        if (!frame) return;
        const reporting = !event.shiftKey && (frame.modes.mouse || (frame.modes.alternate_screen && frame.modes.alternate_scroll));
        const limit = reporting ? 16 : 300;
        const lines = Math.max(-limit, Math.min(limit, Math.trunc(this.wheelLines)));
        this.wheelLines = 0;
        if (reporting) {
          if (lines) this.activate();
          if (!this.viewport.startsWith(`${frame.cols}:${frame.rows}:`)) return;
          for (let i = 0; i < Math.abs(lines); i++) this.mouse(lines < 0 ? 'wheel_up' : 'wheel_down', 3, this.wheelPoint, event);
        } else if (lines) {
          this.selection = null;
          this.input({ kind: 'scroll', lines: -lines });
        }
      });
    }, { signal, passive: false });
    this.observer = new ResizeObserver(() => this.scheduleResize());
    this.observer.observe(host);
    window.addEventListener('resize', () => this.scheduleResize(), { signal });
    void document.fonts.ready.then(() => { if (!this.disposed) this.measure(); });
    this.measure();
  }

  private point(event: MouseEvent): Point {
    const rect = this.canvas.getBoundingClientRect();
    return cellPoint(event.clientX - rect.left, event.clientY - rect.top, this.cellWidth, this.cellHeight, this.frame?.cols ?? 2, this.frame?.rows ?? 2);
  }

  private mouse(action: Extract<Input, { kind: 'mouse' }>['action'], button: number, point: Point, event: MouseEvent) {
    if (!this.frame) return;
    this.input({ kind: 'mouse', geometry: this.frame.geometry, action, button, row: point[0], col: point[1], ctrl: event.ctrlKey, alt: event.altKey, shift: event.shiftKey });
  }

  private pointerdown = (event: PointerEvent) => {
    const frame = this.frame;
    if (!frame) return;
    const point = this.point(event);
    if (event.button === 0 && (event.metaKey || event.ctrlKey)) {
      const link = cellAt(frame, point)?.link;
      const url = link && safeLink(link);
      if (url) { event.preventDefault(); this.options.onLink(url); return; }
    }
    const reporting = frame.modes.mouse && !event.shiftKey && frame.scroll_offset === 0 && (event.button !== 2 || this.rightClickToApp);
    if (!reporting && event.button !== 0) return;
    event.preventDefault(); this.activate(); this.focus();
    if (!this.viewport.startsWith(`${frame.cols}:${frame.rows}:`)) { this.selection = null; return; }
    this.canvas.setPointerCapture(event.pointerId);
    this.pointer = { id: event.pointerId, button: terminalButton(event.button), reporting, geometry: frame.geometry };
    if (reporting) {
      this.selection = null;
      this.mouse('press', this.pointer.button, point, event);
    } else {
      const now = performance.now();
      const count = now - this.click.time < 450 && point[0] === this.click.row && point[1] === this.click.col ? this.click.count % 3 + 1 : 1;
      this.click = { time: now, row: point[0], col: point[1], count };
      this.selection = { anchor: point, cursor: point, mode: count === 3 ? 'line' : count === 2 ? 'word' : 'cell', revision: frame.revision };
    }
    this.schedule();
  };

  private pointermove = (event: PointerEvent) => {
    const frame = this.frame;
    if (!frame) return;
    const pointer = this.pointer;
    if (!pointer) {
      if (frame.modes.mouse_motion && !event.shiftKey && this.viewport.startsWith(`${frame.cols}:${frame.rows}:`)) this.mouse('move', 3, this.point(event), event);
      return;
    }
    if (pointer.id !== event.pointerId || pointer.geometry !== frame.geometry) return;
    if (pointer.reporting) {
      if (frame.modes.mouse_drag) this.mouse('move', pointer.button, this.point(event), event);
    } else if (this.selection?.revision === frame.revision) {
      this.selection.cursor = this.point(event); this.schedule();
    }
  };

  private pointerup = (event: PointerEvent) => {
    const pointer = this.pointer, frame = this.frame;
    if (!pointer || pointer.id !== event.pointerId) return;
    this.pointer = null;
    if (this.canvas.hasPointerCapture(event.pointerId)) this.canvas.releasePointerCapture(event.pointerId);
    if (!frame || frame.geometry !== pointer.geometry) { this.selection = null; return; }
    if (pointer.reporting) this.mouse('release', pointer.button, this.point(event), event);
    else if (this.selection?.revision === frame.revision && event.type !== 'pointercancel') {
      const { anchor, cursor, mode, revision } = this.selection;
      if (mode !== 'cell' || anchor[0] !== cursor[0] || anchor[1] !== cursor[1]) this.input({ kind: 'selection', geometry: frame.geometry, revision, start: anchor, end: cursor, mode });
      else this.selection = null;
    } else this.selection = null;
    this.schedule();
  };

  hasSelection() { return Boolean(this.selection); }
  paste(text: string) { if (!this.disposed && text) this.input({ kind: 'paste', text }); }
  copy() {
    if (this.frame && this.selection) this.input({ kind: 'copy', geometry: this.frame.geometry });
  }

  private activate() {
    this.enqueue({ type: 'focus', id: crypto.randomUUID(), target: this.options.target, activate: true });
  }

  private input(input: Input) {
    if (input.kind === 'paste' && new TextEncoder().encode(input.text).length > 256 * 1024) {
      this.options.onError(new Error('Paste exceeds the core limit of 256 KiB.')); return;
    }
    this.enqueue({ type: 'input', id: crypto.randomUUID(), target: this.options.target, input });
  }

  private enqueue(request: Request) {
    if (this.disposed) return;
    if (this.queue.length >= 256) {
      this.options.onError(new Error('Terminal input is backlogged. Further input was not sent.')); return;
    }
    this.queue.push(request);
    void this.flush();
  }

  private async flush() {
    if (this.sending) return;
    this.sending = true;
    try {
      while (this.queue.length) {
        const batch = takeInputBatch(this.queue);
        try {
          const receipts = await Promise.allSettled(batch.map(request => this.options.send(request)));
          const failure = receipts.find((result, i) => {
            if (result.status !== 'rejected') return false;
            const r = batch[i];
            // A redraw can invalidate pointer coordinates in flight. Never replay them.
            return !(r?.type === 'input' && ['mouse', 'selection', 'copy'].includes(r.input.kind)
              && String(result.reason).endsWith('stale terminal geometry; wait for a fresh frame'));
          });
          if (failure?.status === 'rejected') throw failure.reason;
        }
        catch (error) { if (!this.disposed) { this.queue = []; this.options.onError(error); } }
      }
    } finally { this.sending = false; }
  }

  private keyInput(code: string | undefined, key: Extract<Input, { kind: 'key' }>) {
    const previous = code ? this.held.get(code) : undefined;
    const same = previous && previous.key === key.key && previous.ctrl === key.ctrl && previous.alt === key.alt && previous.shift === key.shift;
    if (previous && !same) this.input({ ...previous, repeat: false, release: true });
    const input = { ...key, repeat: Boolean(same && key.repeat), release: false };
    if (code) this.held.set(code, input);
    this.input(input);
    if (!code) this.input({ ...input, repeat: false, release: true });
  }

  private keydown = (event: KeyboardEvent) => {
    // Ctrl+C remains child input; explicit host copy never reads the clipboard.
    if (this.selection && event.key.toLowerCase() === 'c' && (event.metaKey || (event.ctrlKey && event.shiftKey))) {
      event.preventDefault(); event.stopPropagation(); this.copy(); return;
    }
    if (this.composing) return;
    const route = keyRoute(event);
    if (route === 'ignore') return;
    this.compositionCommit = null;
    // AltGr/Option text is committed once via beforeinput, with matched phases.
    if (route === 'text') {
      this.pendingTextKey = { code: event.code, shift: event.shiftKey, repeat: event.repeat };
      return;
    }
    this.pendingTextKey = null;
    event.preventDefault();
    event.stopPropagation();
    const input = { kind: 'key' as const, key: event.key, ctrl: event.ctrlKey, alt: event.altKey, shift: event.shiftKey, repeat: event.repeat, release: false };
    this.keyInput(event.code, input);
  };

  private keyup = (event: KeyboardEvent) => {
    const input = this.held.get(event.code);
    if (!input) return;
    this.held.delete(event.code);
    event.preventDefault(); this.input({ ...input, repeat: false, release: true });
  };

  private beforeinput = (event: InputEvent) => {
    if (event.isComposing || this.composing || event.inputType === 'insertCompositionText') return;
    event.preventDefault();
    if (event.inputType === 'insertFromPaste') return; // The paste event is authoritative.
    if (!event.data || !event.inputType.startsWith('insert')) return;
    if (this.compositionCommit === event.data) { this.compositionCommit = null; return; }
    this.compositionCommit = null;
    const pending = this.pendingTextKey;
    this.pendingTextKey = null;
    if (event.inputType === 'insertText' && Array.from(event.data).length === 1) {
      const key: Extract<Input, { kind: 'key' }> = { kind: 'key', key: event.data, ctrl: false, alt: false, shift: pending?.shift ?? false, repeat: pending?.repeat ?? false, release: false };
      this.keyInput(pending?.code, key);
    } else this.input({ kind: 'paste', text: event.data });
  };

  focus() { this.textarea.focus({ preventScroll: true }); }

  update(frame: TerminalFrame) {
    if (!Number.isInteger(frame.cols) || !Number.isInteger(frame.rows) || frame.cols < 1 || frame.cols > 500 || frame.rows < 1 || frame.rows > 300 || frame.cells.length > frame.cols * frame.rows) {
      this.options.onError(new Error('The core terminal frame exceeds the supported geometry budget.'));
      return;
    }
    const t = this.options.target, f = frame.target;
    if (f.generation !== t.generation || f.workspace_id !== t.workspace_id || f.tab_id !== t.tab_id || f.pane_id !== t.pane_id || f.terminal_id !== t.terminal_id) return;
    if (this.frame?.revision === frame.revision && this.frame.geometry === frame.geometry) return;
    if (this.selection && this.selection.revision !== frame.revision) this.selection = null;
    this.waiting.remove();
    this.frame = frame; this.images.update(frame.graphics??[]); this.schedule();
  }

  private scheduleResize() {
    if (!this.resizeRaf) this.resizeRaf = requestAnimationFrame(() => { this.resizeRaf = 0; this.measure(); });
  }

  private measure() {
    const context = this.canvas.getContext('2d');
    if (!context) { this.options.onError(new Error('Canvas2D is unavailable.')); return; }
    const style = getComputedStyle(this.host);
    this.foreground = style.getPropertyValue('--terminal-fg').trim() || '#f1f3f7';
    this.background = style.getPropertyValue('--terminal-bg').trim() || '#000000';
    context.font = `${this.fontSize}px ${this.fontFamily}`;
    this.cellWidth = Math.max(1, context.measureText('M').width);
    this.cellHeight = Math.ceil(this.fontSize * 1.45);
    this.width = this.host.clientWidth; this.height = this.host.clientHeight;
    const dpr = window.devicePixelRatio || 1;
    this.canvas.width = Math.round(this.width * dpr); this.canvas.height = Math.round(this.height * dpr);
    this.canvas.style.width = `${this.width}px`; this.canvas.style.height = `${this.height}px`;
    if (this.width > 0 && this.height > 0) {
      const cols = Math.max(2, Math.min(500, Math.floor(this.width / this.cellWidth)));
      const rows = Math.max(2, Math.min(300, Math.floor(this.height / this.cellHeight)));
      const viewport = `${cols}:${rows}:${this.cellWidth}:${this.cellHeight}`;
      if (this.viewport !== viewport) {
        this.viewport = viewport;
        // Coalesce resize intents without discarding ordered input.
        this.queue = this.queue.filter((request) => request.type !== 'viewport');
        this.enqueue({ type: 'viewport', id: crypto.randomUUID(), target: this.options.target, cols, rows, cell_width: Math.round(this.cellWidth), cell_height: this.cellHeight });
      }
    }
    this.schedule();
  }

  private schedule() {
    if (!this.raf) this.raf = requestAnimationFrame(() => { this.raf = 0; if (!this.disposed) this.paint(); });
  }

  private paint() {
    const context = this.canvas.getContext('2d');
    if (!context) return;
    const dpr = window.devicePixelRatio || 1;
    context.setTransform(dpr, 0, 0, dpr, 0, 0);
    context.clearRect(0, 0, this.width, this.height);
    context.globalAlpha = this.backgroundOpacity;
    context.fillStyle = this.background; context.fillRect(0, 0, this.width, this.height);
    context.globalAlpha = 1;
    const frame = this.frame;
    if (!frame) return;
    context.textBaseline = 'alphabetic';
    const baseline = Math.round((this.cellHeight - this.fontSize) / 2) + this.fontSize - 2;
    context.fontKerning = 'none';
    // Paint backgrounds first so a continuation cell cannot erase its wide glyph.
    for (const cell of frame.cells) {
      if (cell.col >= frame.cols || cell.row >= frame.rows) continue;
      const reverse = Boolean(cell.mods & 64);
      const bg = unpackColor(reverse ? cell.fg : cell.bg, reverse ? this.foreground : this.background);
      if (bg !== this.background) {
        context.fillStyle = bg;
        const snap = (pos: number) => Math.round(pos * dpr) / dpr;
        const x = snap(cell.col * this.cellWidth), y = snap(cell.row * this.cellHeight);
        context.fillRect(x, y, snap((cell.col + Math.max(1, cell.width)) * this.cellWidth) - x, snap((cell.row + 1) * this.cellHeight) - y);
      }
    }
    // Keep the font’s natural advance; wide fallback glyphs start at an explicit VT column.
    // No Unicode-width table or second parser exists in this renderer.
    for (const cell of styledRuns(frame.cells)) {
      if (cell.col >= frame.cols || cell.row >= frame.rows) continue;
      if (!cell.text || cell.mods & 128) continue;
      const x = cell.col * this.cellWidth, y = cell.row * this.cellHeight;
      const reverse = Boolean(cell.mods & 64);
      context.fillStyle = unpackColor(reverse ? cell.bg : cell.fg, reverse ? this.background : this.foreground);
      context.globalAlpha = cell.mods & 2 ? 0.6 : 1;
      context.font = `${cell.mods & 4 ? 'italic ' : ''}${cell.mods & 1 ? 'bold ' : ''}${this.fontSize}px ${this.fontFamily}`;
      const advance = cell.width * this.cellWidth;

      context.save();
      context.beginPath(); context.rect(x, y, advance, this.cellHeight); context.clip();
      context.fillText(cell.text, x, y + baseline);
      context.restore();
      if (cell.mods & 8) context.fillRect(x, y + this.cellHeight - 2, advance, 1);
      if (cell.mods & 256) context.fillRect(x, y + this.cellHeight / 2, advance, 1);
    }
    context.globalAlpha = 1;
    this.images.draw(context,this.cellWidth,this.cellHeight,frame);
    if (this.selection) {
      const [start, end] = selectionRange(frame, this.selection.anchor, this.selection.cursor, this.selection.mode);
      context.fillStyle = this.foreground; context.globalAlpha = 0.25;
      for (let row = start[0]; row <= end[0]; row++) {
        const left = row === start[0] ? start[1] : 0;
        const right = row === end[0] ? end[1] + 1 : frame.cols;
        context.fillRect(left * this.cellWidth, row * this.cellHeight, (right - left) * this.cellWidth, this.cellHeight);
      }
      context.globalAlpha = 1;
    }
    const cursor = frame.cursor;
    // IME anchoring is independent of DECTCEM cursor visibility.
    if (cursor && cursor.col < frame.cols && cursor.row < frame.rows) {
      const x = cursor.col * this.cellWidth, y = cursor.row * this.cellHeight;
      context.strokeStyle = this.foreground;
      if (cursor.visible) {
        if (document.activeElement === this.textarea) {
          context.fillStyle = this.foreground; context.globalAlpha = 0.45;
          if (cursor.shape === 'beam') context.fillRect(x, y, 2, this.cellHeight);
          else if (cursor.shape === 'underline') context.fillRect(x, y + this.cellHeight - 2, this.cellWidth, 2);
          else context.fillRect(x, y, this.cellWidth, this.cellHeight);
          context.globalAlpha = 1;
        } else context.strokeRect(x + 0.5, y + 0.5, this.cellWidth - 1, this.cellHeight - 1);
      }
      this.textarea.style.left = `${Math.min(x, Math.max(0, this.width - this.cellWidth))}px`;
      this.textarea.style.top = `${Math.min(y, Math.max(0, this.height - this.cellHeight))}px`;
      this.preedit.style.left = this.textarea.style.left;
      this.preedit.style.top = this.textarea.style.top;
      this.preedit.style.lineHeight = `${this.cellHeight}px`;
    }
    this.accessible.textContent = accessibleText(frame);
  }

  dispose() {
    this.images.dispose();
    this.queue = [];
    for (const key of this.held.values()) this.input({ ...key, repeat: false, release: true });
    this.held.clear(); this.pendingTextKey = null;
    if (this.frame && document.activeElement === this.textarea) this.input({ kind: 'focus_event', focused: false });
    this.disposed = true; this.abort.abort(); this.observer.disconnect();
    cancelAnimationFrame(this.raf); cancelAnimationFrame(this.resizeRaf); cancelAnimationFrame(this.wheelRaf);
    this.canvas.remove(); this.textarea.remove(); this.accessible.remove(); this.waiting.remove(); this.preedit.remove();
  }
}
import { TerminalImages } from './graphics.js';
