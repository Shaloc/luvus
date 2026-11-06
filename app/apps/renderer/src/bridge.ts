import { useEffect, useState, useSyncExternalStore } from 'react';
import type { Request, Response, Target, TerminalFrame } from '@luvus/app-protocol';
import { checkedDomainResult, message, parseSnapshot, record, targetKey, text, type RecordValue, type Snapshot } from './model';

export type Method = Extract<Request, { type: 'action' }>['method'];
export interface DesktopAPI {
  request(request: Request): Promise<unknown>;
  onMessage(callback: (response: Response) => void): () => void;
  reconnect(): Promise<void>;
  bootstrap(): Promise<Response[]>;
  sessions(op:'list'|'switch'|'create'|'stop'|'delete'|'host-add'|'merge'|'open-workspace', name:string | {host:string; session:string; path:string}, generation:string): Promise<unknown>;
  gateway(op:'list'|'start'|'stop'|'copy',kind:'web'|'uhp',control:boolean,generation:string):Promise<unknown>;
  addons(op:'list'|'install'|'uninstall'|'enable'|'disable',agent:string,generation:string):Promise<unknown>;
  terminalPaste(target: Target): Promise<void>;
  chooseDirectory(): Promise<string | null>;
  browserBounds(id: string, bounds: { x: number; y: number; width: number; height: number } | null): Promise<void>;
  browserStatus(id: string): Promise<unknown>;
  browserNavigate(id: string, action: 'navigate' | 'back' | 'forward' | 'reload', bounds: {x:number;y:number;width:number;height:number}, url?: string): Promise<void>;
  browserTakeover(id: string): Promise<void>;
}
declare global { interface Window { luvus: DesktopAPI } }
export interface InboxRow { id: string; text: string; level: string; time: number; count: number }
interface State { fallbackInbox: InboxRow[]; fallbackUnread: number; snapshot: Snapshot | null; ready: boolean; capabilities: string[]; remoteHost?: string; error: string | null; notice: string | null; revision: number; browserTitles: Record<string, string> }
let inboxGeneration = '';
let state: State = { fallbackInbox: [], fallbackUnread: 0, snapshot: null, ready: false, capabilities: [], error: null, notice: null, revision: 0, browserTitles: {} };
const listeners = new Set<() => void>();
const terminals = new Map<string, TerminalFrame>();
const liveFrames = new Set<string>();
let visibleTargets = new Set<string>();
const frameWeights = new Map<string, number>();
let cachedFrameBytes = 0;
const FRAME_CACHE_BYTES = 32 * 1024 * 1024;
function dropFrame(key: string) { terminals.delete(key); liveFrames.delete(key); cachedFrameBytes -= frameWeights.get(key) ?? 0; frameWeights.delete(key); }
const terminalListeners = new Map<string, Set<(frame: TerminalFrame) => void>>();
let disconnect: (() => void) | undefined;
let started = false;
const emit = (patch: Partial<State>) => { state = { ...state, ...patch }; listeners.forEach((callback) => callback()); };

function disconnected(error: string) {
  visibleTargets.clear();
  for (const key of terminals.keys()) dropFrame(key);
  emit({ ready: false, snapshot: null, capabilities: [], error, notice: null, browserTitles: {} });
}
export function markFallbackInboxRead() { if (state.fallbackUnread) emit({ fallbackUnread: 0 }); }

function receive(response: Response) {
  switch (response.type) {
    case 'ready':
      if (response.protocol !== 1 || !response.capabilities.includes('terminal_cells.v1')) {
        emit({ ready: false, error: 'This core does not advertise the required desktop terminal protocol. Upgrade it before attaching.' });
      } else emit({ ready: true, capabilities: response.capabilities, error: null });
      break;
    case 'snapshot': {
      try {
        if (!state.ready) return;
        const snapshot = parseSnapshot(response.snapshot);
        const newSession = inboxGeneration !== snapshot.generation;
        inboxGeneration = snapshot.generation;
        const valid = new Set(snapshot.workspaces.filter((w) => w.active).flatMap((w) => w.tabs.filter((t) => t.active).flatMap((t) => t.panes.filter((p) => !t.zoomed || p.id === t.focus).flatMap((p) => p.target ? [targetKey(p.target)] : []))));
        visibleTargets = valid;
        // Keep paint-only previews for sibling tabs. Never reuse another workspace or remote projection.
        const keep = new Set(snapshot.workspaces.filter(w => w.active && !w.host).flatMap(w => w.tabs.flatMap(t => t.panes.flatMap(p => p.target?.terminal_id ? [targetKey(p.target)] : []))));
        for (const key of terminals.keys()) {
          if (!valid.has(key)) liveFrames.delete(key);
          if (!valid.has(key) && !keep.has(key)) dropFrame(key);
        }
        const browsers = new Set(snapshot.workspaces.flatMap(w => w.tabs.flatMap(t => t.panes.filter(p => p.kind === 'browser').map(p => p.id))));
        const browserTitles = snapshot.generation === state.snapshot?.generation ? Object.fromEntries(Object.entries(state.browserTitles).filter(([id]) => browsers.has(id))) : {};
        emit({ snapshot, revision: state.revision + 1, browserTitles, ...(newSession ? { fallbackInbox: [], fallbackUnread: 0 } : {}) });
      } catch (error) { emit({ error: message(error) }); }
      break;
    }
    case 'terminal': {
      if (!state.ready) return;
      const frame = response.frame;
      if (state.snapshot && frame.target.generation !== state.snapshot.generation) return;
      const key = targetKey(frame.target);
      if (state.snapshot && !visibleTargets.has(key)) return;
      if (liveFrames.has(key) && terminals.get(key)?.revision === frame.revision && terminals.get(key)?.geometry === frame.geometry) return;
      if (frame.cols < 1 || frame.cols > 500 || frame.rows < 1 || frame.rows > 300 || frame.cells.length > frame.cols * frame.rows) { emit({ error: 'The core terminal frame exceeds the desktop geometry budget.' }); return; }
      const weight = frame.cells.reduce((size, cell) => size + 64 + cell.text.length * 2 + (cell.link?.length ?? 0) * 2, 0)
        + (frame.graphics??[]).reduce((n,g)=>n+g.cells.length*64,0);
      dropFrame(key);
      while (terminals.size && (terminals.size >= 128 || cachedFrameBytes + weight > FRAME_CACHE_BYTES)) dropFrame(terminals.keys().next().value!);
      if (weight <= FRAME_CACHE_BYTES) { terminals.set(key, frame); liveFrames.add(key); frameWeights.set(key, weight); cachedFrameBytes += weight; }
      terminalListeners.get(key)?.forEach((callback) => callback(frame));
      break;
    }
    case 'error':
      if (!response.id) {
        if (['connection_failed', 'incompatible_protocol', 'startup_failed'].includes(response.code)) disconnected(response.message);
        else reportError(response.message);
      }
      break;
    case 'effect': {
      const data = record(response.data);
      if (response.kind === 'browser_status') {
        const target = record(data.target);
        const pane = text(target.pane_id), title = text(data.title).slice(0, 2048);
        if (target.generation === state.snapshot?.generation && pane && title && title !== state.browserTitles[pane]
            && (Object.hasOwn(state.browserTitles, pane) || Object.keys(state.browserTitles).length < 128)) emit({ browserTitles: { ...state.browserTitles, [pane]: title } });
        break;
      }
      if (response.kind === 'direct_ssh') { emit({ remoteHost: text(data.host) }); break; }
      if (['disconnected', 'shutdown', 'core_exit'].includes(response.kind)) disconnected(text(data.message) || text(data.reason) || 'The core connection closed.');
      else if (['toast', 'notice', 'notification'].includes(response.kind)) {
        const body = (text(data.text) || text(data.message) || text(data.body) || text(response.data)).slice(0, 4096);
        if (!body) break;
        if (state.snapshot && !state.snapshot.raw.notification_inbox) {
          const row = { id: crypto.randomUUID(), text: body, level: text(data.level, 'info'), time: Math.floor(Date.now()/1000), count: 1 };
          emit({ fallbackInbox: [row, ...state.fallbackInbox].slice(0,100), fallbackUnread: Math.min(100,state.fallbackUnread + 1) });
        }
        if (response.kind !== 'notification') notify(body);
      }
      break;
    }
    // Reply promises and chunk assembly belong to the native bridge.
    default: break;
  }
}
export async function connect() {
  if (started) return;
  started = true;
  if (!window.luvus) { emit({ error: 'The desktop bridge is unavailable. Open this interface in the Luvus desktop app.' }); return; }
  let booting = true;
  const buffered: Response[] = [];
  disconnect = window.luvus.onMessage((response) => {
    if (booting) {
      // Bootstrap is bounded; coalesce visual messages while retaining effects.
      if (response.type === 'terminal') {
        const key = targetKey(response.frame.target);
        const i = buffered.findIndex((item) => item.type === 'terminal' && targetKey(item.frame.target) === key);
        if (i >= 0) buffered.splice(i, 1);
      } else if (response.type === 'snapshot') {
        const i = buffered.findIndex((item) => item.type === 'snapshot'); if (i >= 0) buffered.splice(i, 1);
      }
      if (buffered.length < 256) buffered.push(response);
    } else receive(response);
  });
  try { (await window.luvus.bootstrap()).forEach(receive); }
  catch (error) { emit({ error: message(error) }); }
  finally { booting = false; buffered.forEach(receive); }
}
window.addEventListener('pagehide', () => disconnect?.());
export const useCore = () => useSyncExternalStore((callback) => { listeners.add(callback); return () => { listeners.delete(callback); }; }, () => state);
export const reportError = (error: unknown) => { if (state.ready || !state.error) emit({ error: message(error) }); };
let noticeTimer: ReturnType<typeof setTimeout> | undefined;
export function notify(notice: string) {
  clearTimeout(noticeTimer); emit({ notice });
  noticeTimer = setTimeout(() => emit({ notice: null }), 4000);
}
export const clearError = () => emit({ error: null });
export const clearNotice = () => emit({ notice: null });
export const currentSnapshot = () => state.snapshot;
export async function request(request: Request): Promise<unknown> {
  if (!window.luvus || !state.ready) throw new Error(state.error || 'The desktop core is not connected.');
  return window.luvus.request(request);
}
export async function action(method: Method, params: RecordValue = {}, generation = state.snapshot?.generation): Promise<unknown> {
  if (!generation || generation !== state.snapshot?.generation) throw new Error('The server changed. Reopen this action and try again.');
  return request({ type: 'action', id: crypto.randomUUID(), generation, method, params });
}
export async function refreshSnapshot() { await request({ type: 'snapshot', id: crypto.randomUUID() }); }
export async function mutate(method: Method, params: RecordValue = {}, generation?: string) {
  const result = await action(method, params, generation); await refreshSnapshot(); return result;
}
export async function focusTarget(target: Target, activate = true) {
  await request({ type: 'focus', id: crypto.randomUUID(), target, activate });
  if (activate) await refreshSnapshot();
}
export function subscribeTerminal(target: Target, callback: (frame: TerminalFrame) => void, preview?: (frame: TerminalFrame) => void) {
  const key = targetKey(target);
  let set = terminalListeners.get(key);
  if (!set) { set = new Set(); terminalListeners.set(key, set); }
  set.add(callback);
  const frame = terminals.get(key); if (frame) (liveFrames.has(key) ? callback : preview)?.(frame);
  return () => { set.delete(callback); if (!set.size) terminalListeners.delete(key); };
}
export function useQuery(method: Method, params: RecordValue = {}, enabled = true, retainOnReload = false) {
  const { snapshot } = useCore();
  const [result, setResult] = useState<{ data: unknown; error: string | null; loading: boolean }>({ data: null, error: null, loading: enabled });
  const [refresh, setRefresh] = useState(0);
  const key = JSON.stringify(params), generation = snapshot?.generation;
  const workspaceId = snapshot?.workspaces.find((w) => w.active)?.id;
  useEffect(() => {
    let live = true;
    if (!enabled || !generation) return;
    setResult(previous => retainOnReload && previous.data !== null ? previous : { data: null, error: null, loading: true });
    void action(method, JSON.parse(key) as RecordValue, generation).then((data) => checkedDomainResult(method, data)).then(
      (data) => { if (live) setResult({ data, error: null, loading: false }); },
      (error: unknown) => { if (live) setResult({ data: null, error: message(error), loading: false }); },
    );
    return () => { live = false; };
  }, [method, key, generation, workspaceId, refresh, enabled, retainOnReload]);
  return { ...result, reload: () => setRefresh((value) => value + 1) };
}
