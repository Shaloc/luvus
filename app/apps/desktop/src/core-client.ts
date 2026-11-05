import { EventEmitter } from 'node:events';
import { execFile, spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import { promisify } from 'node:util';
import { encodeRequest, ResponseDecoder, type Request, type Response } from '@luvus/app-protocol';

const execute = promisify(execFile);

export class IncompatibleCoreError extends Error {}

export class CoreClient extends EventEmitter {
  private child: ChildProcessWithoutNullStreams | undefined;
  private readonly pending = new Map<string, { resolve: (value: unknown) => void; reject: (error: Error) => void; timer: NodeJS.Timeout }>();
  private readonly latest = new Map<string, Response>();
  private closing = false;
  private closePromise: Promise<void> | undefined;

  constructor(readonly binary: string, readonly session: string, private readonly environment: NodeJS.ProcessEnv,
    private readonly remote?: string) { super(); }

  async start(): Promise<void> {
    if (this.child) throw new Error('Core attachment already exists');
    const env = { ...this.environment };
    for (const name of ['LUVUS_SOCKET_PATH', 'LUVUS_SESSION', 'LUVUS_REMOTE_HOST', 'LUVUS_REMOTE_SESSION', 'LUVUS_PANE_ID', 'LUVUS_API_ADDRESS']) delete env[name];
    if (!this.remote) {
      await execute(this.binary, ['--session', this.session, 'server', 'start'], { env, timeout: 15000, maxBuffer: 1024 * 1024, windowsHide: true });
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
    child.on('exit', () => {
      if (this.child !== child) return;
      this.child = undefined;
      if (!this.closing) this.fail(new Error(diagnostic.trim() || 'The selected core attachment closed'));
    });
  }

  bootstrap(): Response[] { return [...this.latest.values()]; }

  request(request: Request): Promise<unknown> {
    const child = this.child;
    if (!child || this.closing) return Promise.reject(new Error('The core is not connected'));
    if (this.pending.size >= 32 || this.pending.has(request.id)) return Promise.reject(new Error('The attachment request queue is full'));
    let bytes: Uint8Array;
    try { bytes = encodeRequest(request); }
    catch (error) { return Promise.reject(error); }
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(request.id);
        reject(new Error('Core response timed out; the execution outcome may be unknown'));
      }, 35000);
      this.pending.set(request.id, { resolve, reject, timer });
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
    if (!child) return;
    await new Promise<void>(resolve => {
      const timeout = setTimeout(() => { child.kill('SIGTERM'); resolve(); }, 5000);
      child.once('exit', () => { clearTimeout(timeout); resolve(); });
      if (child.stdin.writable) child.stdin.end(encodeRequest({ type: 'detach', id: crypto.randomUUID() }));
      else child.kill('SIGTERM');
    });
    this.child = undefined;
  }

  private receive(message: Response): void {
    if (message.type === 'reply' || message.type === 'error' && message.id) {
      const id = message.id;
      const pending = id ? this.pending.get(id) : undefined;
      if (pending) {
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
  }

  private fail(error: unknown): void {
    const message = error instanceof Error ? error.message : 'Core connection failed';
    const response: Response = { type: 'error', id: null, code: 'connection_failed', message };
    this.latest.set('error', response);
    this.emit('message', response);
    for (const pending of this.pending.values()) { clearTimeout(pending.timer); pending.reject(new Error(message)); }
    this.pending.clear();
  }
}
