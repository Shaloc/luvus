import { EventEmitter } from 'node:events';
import { execFile, spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import { promisify } from 'node:util';
import { LEGACY_BLANK_TAB } from './browser-policy.js';
import { encodeRequest, ResponseDecoder, type Request, type Response } from '@luvus/app-protocol';

const execute = promisify(execFile);

export class IncompatibleCoreError extends Error {}

export class CoreClient extends EventEmitter {
  private child: ChildProcessWithoutNullStreams | undefined;
  private readonly pending = new Map<string, { resolve: (value: unknown) => void; reject: (error: Error) => void; timer: NodeJS.Timeout; snapshot: boolean; acknowledged: boolean }>();
  private readonly latest = new Map<string, Response>();
  private closing = false;
  private failed = false;
  private closePromise: Promise<void> | undefined;

  constructor(readonly binary: string, readonly session: string, private readonly environment: NodeJS.ProcessEnv,
    private readonly remote?: string) { super(); }

  async start(existingOnly = false): Promise<void> {
    if (this.child) throw new Error('Core attachment already exists');
    const env = { ...this.environment };
    for (const name of ['LUVUS_SOCKET_PATH', 'LUVUS_SESSION', 'LUVUS_REMOTE_HOST', 'LUVUS_REMOTE_SESSION', 'LUVUS_PANE_ID', 'LUVUS_API_ADDRESS']) delete env[name];
    if (!this.remote) {
      if (!existingOnly) await execute(this.binary, ['--session', this.session, 'server', 'start'], { env, timeout: 15000, maxBuffer: 1024 * 1024, windowsHide: true });
      // A running released core can have the same package version as this
      // bundle while lacking HelloApp. Ask its JSON API before binary attach.
      const { stdout } = await execute(this.binary, ['--session', this.session, 'uhp', 'capabilities'], { env, timeout: 10000, maxBuffer: 2 * 1024 * 1024, windowsHide: true });
      const display = JSON.parse(stdout).result?.remote_display;
      if (!Array.isArray(display?.compatible_transports) || !display.compatible_transports.includes(21)
        || !Array.isArray(display.capabilities) || !display.capabilities.includes('workspace_app.v1')) {
        throw new IncompatibleCoreError(`Session "${this.session}" is running a core without desktop support (transport ${Number.isInteger(display?.transport) ? display.transport : 'unknown'}; this app requires 21). Its terminals are still running.`);
      }
    }
    if (this.closing) return;
    const args = ['--session', this.session, 'client', '--surface', 'app', '--stdio'];
    if (this.remote) args.push('--remote', this.remote);
    if (this.remote) {
      const origin: Response = { type: 'effect', kind: 'direct_ssh', data: { host: this.remote } };
      this.latest.set('origin', origin);
      this.emit('message', origin);
    }
    const child = spawn(this.binary, args, { env, stdio: 'pipe', windowsHide: true });
    this.child = child;
    const decoder = new ResponseDecoder(message => this.receive(message));
    child.stdout.on('data', (chunk: Buffer) => {
      try { decoder.push(chunk); }
      catch (error) { this.fail(error); void this.close(); }
    });
    let diagnostic = '';
    child.stderr.on('data', (chunk: Buffer) => { diagnostic = (diagnostic + chunk.toString('utf8')).slice(-4096); });
    child.on('error', error => this.fail(error));
    child.stdin.on('error', error => { if (!this.closing) this.fail(error); });
    child.on('close', (code, signal) => {
      if (this.child !== child) return;
      this.child = undefined;
      if (!this.closing) {
        const status = signal || `exit ${code ?? 'unknown'}`;
        try { decoder.finish(); } catch (error) { this.fail(error); }
        this.fail(new Error(diagnostic.trim() || `Desktop connection to "${this.session}" closed (${status}).`));
      }
    });
  }

  bootstrap(): Response[] { return [...this.latest.values()]; }

  request(request: Request): Promise<unknown> {
    const child = this.child;
    if (!child || this.failed || this.closing) return Promise.reject(new Error('The core is not connected'));
    if (this.pending.size >= 32 || this.pending.has(request.id)) return Promise.reject(new Error('The attachment request queue is full'));
    if (request.type === 'browser_tab' && request.url === 'about:blank') {
      const snapshot = this.latest.get('snapshot');
      const features = snapshot?.type === 'snapshot' && (snapshot.snapshot as Record<string,unknown>).desktop_features;
      if (!Array.isArray(features) || !features.includes('browser_tab')) return Promise.reject(new Error('This session does not support browser tabs.'));
      // Sending about:blank to an early v21 owner terminates the whole desktop transport.
      if (!features.includes('blank_browser_tab')) request = { ...request, url: LEGACY_BLANK_TAB };
    }
    let bytes: Uint8Array;
    try { bytes = encodeRequest(request); }
    catch (error) { return Promise.reject(error); }
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(request.id);
        reject(new Error('Core response timed out; the execution outcome may be unknown'));
      }, 35000);
      this.pending.set(request.id, { resolve, reject, timer, snapshot: request.type==='snapshot', acknowledged:false });
      child.stdin.write(bytes, error => {
        if (!error) return;
        clearTimeout(timer);
        this.pending.delete(request.id);
        reject(error);
      });
    });
  }

  close(): Promise<void> {
    return this.closePromise ??= this.closeInner();
  }

  private async closeInner(): Promise<void> {
    this.closing = true;
    for (const pending of this.pending.values()) { clearTimeout(pending.timer); pending.reject(new Error('Core attachment closed')); }
    this.pending.clear();
    const child = this.child;
    if (!child || child.exitCode !== null || child.signalCode !== null) return;
    await new Promise<void>(resolve => {
      const timeout = setTimeout(() => { child.kill('SIGTERM'); resolve(); }, 5000);
      child.once('exit', () => { clearTimeout(timeout); resolve(); });
      if (child.stdin.writable) child.stdin.end(encodeRequest({ type: 'detach', id: crypto.randomUUID() }));
      else child.kill('SIGTERM');
    });
    this.child = undefined;
  }

  private receive(message: Response): void {
    if (this.failed || this.closing) return;
    if (message.type === 'effect' && ['disconnected', 'shutdown', 'core_exit'].includes(message.kind)) {
      const data = message.data as { message?: string; reason?: string } | null;
      this.fail(new Error(data?.message || data?.reason || 'The selected session disconnected.'));
      return;
    }
    if (message.type === 'error' && !message.id && ['resource_limit','connection_failed','incompatible_protocol'].includes(message.code)) {
      this.fail(new Error(message.message));
      return;
    }
    if (message.type === 'reply' || message.type === 'error' && message.id) {
      const id = message.id;
      const pending = id ? this.pending.get(id) : undefined;
      if (pending) {
        // The owner acknowledges snapshot requests before rendering them. Keep
        // callers behind that frame, so focus and input use the committed view.
        if (pending.snapshot && message.type==='reply') {pending.acknowledged=true;return;}
        clearTimeout(pending.timer);
        this.pending.delete(id!);
        if (message.type === 'error') pending.reject(new Error(message.message));
        else pending.resolve(message.result);
      }
      return;
    }
    if (message.type === 'snapshot') {
      this.latest.set('snapshot', message);
      const model = message.snapshot as Record<string, unknown>;
      const workspaces = Array.isArray(model.workspaces) ? model.workspaces : [];
      const workspace = workspaces.find((value: Record<string, unknown>) => value.active);
      const tabs = workspace && Array.isArray(workspace.tabs) ? workspace.tabs : [];
      const tab = tabs.find((value: Record<string, unknown>) => value.active);
      const panes = tab && Array.isArray(tab.panes) ? tab.panes : [];
      const visible = new Set(panes.map((value: Record<string, unknown>) => String(value.pane_id)));
      for (const key of this.latest.keys()) if (key.startsWith('terminal:') && !visible.has(key.slice(9))) this.latest.delete(key);
    } else if (message.type === 'terminal') this.latest.set(`terminal:${message.frame.target.pane_id}`, message);
    else if (message.type === 'ready') this.latest.set('ready', message);
    this.emit('message', message);
    if (message.type==='snapshot') {
      for (const [id,pending] of this.pending) if (pending.snapshot && pending.acknowledged) {
        clearTimeout(pending.timer);this.pending.delete(id);pending.resolve(message.snapshot);
      }
    }
  }

  private fail(error: unknown): void {
    if (this.failed || this.closing) return;
    this.failed = true;
    this.latest.clear();
    const message = error instanceof Error ? error.message : 'Core connection failed';
    const response: Response = { type: 'error', id: null, code: 'connection_failed', message };
    this.latest.set('error', response);
    this.emit('message', response);
    for (const pending of this.pending.values()) { clearTimeout(pending.timer); pending.reject(new Error(message)); }
    this.pending.clear();
  }
}
