import type { Target } from '@luvus/app-protocol';

export type RecordValue = Record<string, unknown>;
export const record = (value: unknown): RecordValue => value !== null && typeof value === 'object' && !Array.isArray(value) ? value as RecordValue : {};
export const array = (value: unknown): unknown[] => Array.isArray(value) ? value : [];
export const text = (value: unknown, fallback = ''): string => typeof value === 'string' ? value : fallback;
export const numeric = (value: unknown, fallback = 0): number => typeof value === 'number' && Number.isFinite(value) ? value : fallback;
export const truth = (value: unknown): boolean => value === true;
export const page = (value: unknown): RecordValue[] => array(record(value).items).map(record);
export const message = (error: unknown): string => error instanceof Error ? error.message : text(error, 'The operation could not be completed.');
export const label = (value: string): string => value.replaceAll('_', ' ').replaceAll('.', ' › ');

export type Layout = { Leaf: number | string } | { Split: { axis: 0 | 1; ratio: number; a: Layout; b: Layout } };
export interface Pane {
  id: string; kind: string; title: string; focused: boolean; target: Target | null;
  cwd: string; agent: string; agentName: string; agentStatus: string; isAgent: boolean;
  data: RecordValue; raw: RecordValue;
}
export interface Tab { id: string; index1: number; name: string; kind: string; active: boolean; focus: string; zoomed: boolean; layout: Layout | null; panes: Pane[] }
export interface Workspace { id: string; index1: number; name: string; cwd: string; host: string; branch: string; active: boolean; pinned: boolean; parentId: string; order: number; guiAvailable: boolean; tabs: Tab[]; raw: RecordValue }
export interface Snapshot { generation: string; session: string; workspaces: Workspace[]; config: RecordValue; commands: RecordValue[]; raw: RecordValue }

export function parseTarget(value: unknown): Target | null {
  const r = record(value);
  if (!['generation', 'workspace_id', 'tab_id', 'pane_id'].every((key) => typeof r[key] === 'string' && r[key].length > 0)) return null;
  if (r.terminal_id !== null && typeof r.terminal_id !== 'string') return null;
  return { generation: text(r.generation), workspace_id: text(r.workspace_id), tab_id: text(r.tab_id), pane_id: text(r.pane_id), terminal_id: r.terminal_id as string | null };
}
export function parseLayout(value: unknown, depth = 0): Layout | null {
  if (depth > 64) return null;
  const r = record(value);
  if (typeof r.Leaf === 'string' && r.Leaf.startsWith('r:')) return { Leaf: r.Leaf };
  if (Number.isSafeInteger(r.Leaf) && numeric(r.Leaf, -1) >= 0) return { Leaf: numeric(r.Leaf) };
  const s = record(r.Split);
  if (s.axis !== 0 && s.axis !== 1) return null;
  const a = parseLayout(s.a, depth + 1), b = parseLayout(s.b, depth + 1);
  if (!a || !b || typeof s.ratio !== 'number' || s.ratio <= 0 || s.ratio >= 1) return null;
  return { Split: { axis: s.axis, ratio: s.ratio, a, b } };
}
export function parseSnapshot(value: unknown): Snapshot {
  const r = record(value);
  if (!text(r.server_generation) || !Array.isArray(r.workspaces)) throw new Error('The core did not provide a compatible GUI snapshot. Upgrade the selected core; it will not be restarted automatically.');
  const workspaces = r.workspaces.map((value): Workspace => {
    const w = record(value);
    if (!text(w.id) || !Number.isSafeInteger(w.index) || numeric(w.index) < 1) throw new Error('Invalid workspace identity in GUI snapshot.');
    const tabs = array(w.tabs).map((value): Tab => {
      const t = record(value);
      return {
        id: text(t.id), index1: numeric(t.index), name: text(t.name), kind: text(t.kind), active: truth(t.active),
        focus: text(t.focus), zoomed: truth(t.zoomed), layout: parseLayout(t.layout),
        panes: array(t.panes).map((value): Pane => {
          const p = record(value), target = parseTarget(p.target);
          const validTarget = target && target.generation === r.server_generation && target.workspace_id === w.id && target.tab_id === t.id && target.pane_id === p.pane_id ? target : null;
          return { id: text(p.pane_id), kind: text(p.kind), title: text(p.agent_name) || text(p.agent_session_title) || text(p.title) || 'Terminal', focused: truth(p.focused), target: validTarget, cwd: text(p.cwd), agent: text(p.agent), agentName: text(p.agent_name), agentStatus: text(p.agent_status), isAgent: truth(p.is_agent), data: record(p.data), raw: p };
        }),
      };
    });
    return { id: text(w.id), index1: numeric(w.index), name: text(w.name), cwd: text(w.cwd), host: text(w.host), branch: text(w.branch), active: truth(w.active), pinned: truth(w.pinned), parentId: text(w.parent_id), order: numeric(w.sidebar_order), guiAvailable: w.gui_available === true, tabs, raw: w };
  });
  return { generation: text(r.server_generation), session: text(r.session, 'default'), workspaces, config: record(r.config), commands: array(r.commands).map(record), raw: r };
}
export function checkedDomainResult(method: string, value: unknown): unknown {
  const r = record(value);
  const arrays: Record<string, string[]> = {
    'files.tree': ['rows'], 'git.status': ['staged', 'unstaged', 'untracked'], 'git.branches': ['branches'], 'git.log': ['commits'],
    'diff.list': ['files'], 'diff.get': ['hunks'], 'diff.note.list': ['notes'], 'agent.sessions': ['sessions'],
    'mission.snapshot': ['rows'], 'task.list': ['tasks'], 'automation.list': ['automations'], 'module.list': ['modules'],
    'module.settings.list': ['settings'], 'theme.list': ['themes'], 'uhp.capabilities': ['methods'],
  };
  if (value === null || typeof value !== 'object' || Array.isArray(value)) throw new Error(`${method} returned an invalid domain result.`);
  if ((arrays[method] ?? []).some((field) => !Array.isArray(r[field]))) throw new Error(`${method} returned a result without its expected collection.`);
  if (method === 'mission.snapshot' && (typeof record(r.summary).agents !== 'number' || typeof record(record(r.summary).usage_coverage).total !== 'number')) throw new Error('Mission Control did not provide a valid usage summary.');
  return value;
}

export const targetKey = (target: Target): string => JSON.stringify([target.generation, target.workspace_id, target.tab_id, target.pane_id, target.terminal_id]);
export const workspaceParams = (workspace: Workspace): RecordValue => ({ workspace_id: workspace.id });
