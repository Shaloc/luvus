import { tr } from './i18n';
import { useEffect, useRef, useState } from 'react';
import { Bot, ChevronDown, ChevronRight, Folder, GitBranch, MoreHorizontal, Monitor, Pin, Plus, Search, Server, Settings2, Terminal, Workflow } from 'lucide-react';
import { focusTarget, mutate, reportError, useCore } from './bridge';
import { openWorkspaceDialog, topologyDialog, type OpenDialog } from './Actions';
import { record, text, targetKey, type Pane, type Snapshot, type Workspace } from './model';
import { Choice, IconButton, StatusDot } from './ui';
import { SessionSelector } from './SessionSelector';

export type Tool = 'files' | 'diff' | 'agents' | 'git' | 'mission' | 'orch' | 'modules' | 'settings' | 'tools' | 'connection' | 'search';
interface Props { snapshot: Snapshot; openDialog: OpenDialog; setTool: (tool: Tool) => void; onAgent: (pane: Pane) => void; onPalette: () => void }
const states = ['all', 'active', 'working', 'blocked', 'idle', 'done', 'unknown'];
const preferenceKey = 'luvus.desktop.sidebar.v1';
function savedPreferences() {
  try { const value = JSON.parse(localStorage.getItem(preferenceKey) ?? '{}'); return value && typeof value === 'object' ? value : {}; } catch { return {}; }
}
function agentDate(pane: Pane) {
  const value = pane.raw.agent_started_at;
  if (typeof value !== 'number' || !Number.isFinite(value) || value <= 0) return 'Unknown date';
  return new Date(value * 1000).toLocaleDateString();
}
export function Sidebar({ snapshot, openDialog, setTool, onAgent, onPalette }: Props) {
  const { remoteHost } = useCore();
  const [saved] = useState(savedPreferences);
  const [filter, setFilter] = useState<string>(states.includes(saved.filter) ? saved.filter : 'all'), [scope, setScope] = useState(saved.scope === true);
  const [group, setGroup] = useState<string>(['workspace', 'state', 'date', 'none'].includes(saved.group) ? saved.group : 'workspace'), [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const [ratio, setRatio] = useState(Number.isFinite(saved.ratio) ? Math.max(25, Math.min(75, saved.ratio)) : 52), split = useRef<HTMLDivElement>(null);
  useEffect(() => { localStorage.setItem(preferenceKey, JSON.stringify({ filter, scope, group, ratio })); }, [filter, scope, group, ratio]);
  const workspaces = [...snapshot.workspaces].sort((a, b) => a.order - b.order);
  const hosts = [...new Set(workspaces.map(workspace => workspace.host))];
  const layout = record(snapshot.config.layout);
  const agentName = (p:Pane) => (layout.agent_title_first === true && text(p.raw.agent_session_title)) || p.agentName || p.agent || p.title;
  const agentDetail = (p:Pane,w:Workspace) => {
    if (layout.agent_paths === false) return '';
    const title = layout.agent_title === true && !layout.agent_title_first && text(p.raw.agent_session_title);
    if (title) return title;
    const path = p.cwd || w.cwd;
    if (path === w.cwd) return path.split('/').filter(Boolean).at(-1) || path;
    return w.cwd && path.startsWith(w.cwd + '/') ? './' + path.slice(w.cwd.length + 1) : path;
  };
  const agents = workspaces.filter(w => !scope || w.active).flatMap(workspace => workspace.tabs.flatMap(tab => tab.panes.filter(pane => pane.isAgent).map(pane => ({ workspace, pane })))).filter(({ pane }) => filter === 'all' || (filter === 'active' ? ['working', 'busy', 'running', 'waiting'].includes(pane.agentStatus) : pane.agentStatus === filter));
  const groups = new Map<string, { label: string; host: string; rows: typeof agents }>();
  for (const row of agents) {
    const key = group === 'workspace' ? row.workspace.id : group === 'state' ? row.pane.agentStatus : group === 'date' ? agentDate(row.pane) : 'all';
    if (!groups.has(key)) groups.set(key, { label: group === 'workspace' ? row.workspace.name : group === 'state' ? row.pane.agentStatus : group === 'date' ? key : '', host: group === 'workspace' ? row.workspace.host || remoteHost || '' : '', rows: [] });
    groups.get(key)!.rows.push(row);
  }
  const toggle = (key: string) => setCollapsed(current => { const next = new Set(current); if (next.has(key)) next.delete(key); else next.add(key); return next; });
  const workspaceRow = (workspace: Workspace) => <div className={`workspace-row ${workspace.active ? 'active' : ''} ${workspace.parentId ? 'worktree-row' : ''}`} key={workspace.id}>
    <button className="workspace-main" onClick={() => { void mutate('workspace.focus', { workspace_id: workspace.id }, snapshot.generation).catch(reportError); }} title={workspace.cwd} aria-current={workspace.active ? 'page' : undefined}>
      {workspace.parentId ? <GitBranch size={14} /> : <Folder size={14} />}<span className="row-copy"><strong>{workspace.name}</strong></span>{workspace.branch && <small className="workspace-branch">{workspace.branch}</small>}{workspace.pinned && <Pin size={11} />}
    </button>
    <details className="dropdown"><summary className="icon-button" aria-label={`Actions for ${workspace.name}`}><MoreHorizontal size={14} /></summary><div className="dropdown-menu">
      <button onClick={() => openDialog(topologyDialog('workspace-rename', snapshot, workspace))}>{tr("Rename…")}</button>
      <button onClick={() => { void mutate('workspace.pin', { workspace_id: workspace.id, pinned: !workspace.pinned }, snapshot.generation).catch(reportError); }}>{workspace.pinned ? tr("Unpin") : tr("Pin workspace")}</button>
      <button className="destructive-text" onClick={() => openDialog(topologyDialog('workspace-close', snapshot, workspace))}>{tr("Close workspace…")}</button>
    </div></details>
  </div>;
  return <aside className="sidebar" aria-label={tr("Workspaces and agents")}>
    <div className="sidebar-titlebar"><span className="app-wordmark">Luvus</span><IconButton label={tr("Find a command… ⌘ K")} onClick={onPalette}><Search size={15} /></IconButton></div>
    <SessionSelector snapshot={snapshot} openDialog={openDialog} onManage={() => setTool('connection')} />
    <div className="sidebar-sections" ref={split}>
      <section className="workspace-section" aria-label={tr("Workspaces")} style={{ flex: `${ratio} 1 0` }}>
        <div className="sidebar-section-heading"><span>{tr("Workspaces")}</span><IconButton label={tr("Open workspace")} onClick={() => openDialog(openWorkspaceDialog())}><Plus size={14} /></IconButton></div>
        <nav className="section-scroll" aria-label={tr("Workspaces")}>{hosts.map(host => <div key={host || 'local'}>
          <div className="host-row"><button className="sidebar-group" aria-expanded={!collapsed.has(`host:${host}`)} onClick={() => toggle(`host:${host}`)}>{collapsed.has(`host:${host}`) ? <ChevronRight size={12} /> : <ChevronDown size={12} />}{host || remoteHost ? <Server size={13} /> : <Monitor size={13} />}<span>{host || remoteHost || tr("This computer")}</span><small>{workspaces.filter(w => w.host === host).length}</small></button><IconButton label={`Open workspace on ${host || remoteHost || 'this computer'}`} onClick={() => openDialog(openWorkspaceDialog(host || undefined))}><Plus size={12}/></IconButton></div>
          {!collapsed.has(`host:${host}`) && workspaces.filter(w => w.host === host).map(workspaceRow)}
        </div>)}{!workspaces.length && <p className="sidebar-empty">{tr("No workspaces")}</p>}</nav>
      </section>
      <div className="sidebar-section-resizer" role="separator" tabIndex={0} aria-label={tr("Workspace and agent sections")} aria-orientation="horizontal" aria-valuemin={25} aria-valuemax={75} aria-valuenow={ratio}
        onKeyDown={event => { if (['ArrowUp', 'ArrowDown'].includes(event.key)) { event.preventDefault(); setRatio(value => Math.max(25, Math.min(75, value + (event.key === 'ArrowUp' ? -5 : 5)))); } }}
        onPointerDown={event => event.currentTarget.setPointerCapture(event.pointerId)} onPointerMove={event => { if (!event.currentTarget.hasPointerCapture(event.pointerId)) return; const rect = split.current!.getBoundingClientRect(); setRatio(Math.max(25, Math.min(75, (event.clientY - rect.top) / rect.height * 100))); }} onPointerUp={event => event.currentTarget.releasePointerCapture(event.pointerId)} />
      <section className="agent-section" aria-label={tr("Agents")} style={{ flex: `${100 - ratio} 1 0` }}>
        <div className="sidebar-section-heading"><span>{tr("Agents")} <span className="count">{agents.length}</span></span><IconButton label={tr("Agent details and history")} onClick={() => setTool('agents')}><Bot size={14} /></IconButton></div>
        <div className="agent-filters"><Choice label={tr("Filter agents by state")} value={filter} onChange={setFilter} options={states.map(value => ({ value, label: value === 'all' ? tr("All states") : document.documentElement.lang === 'zh-CN' ? tr(value) : value.charAt(0).toUpperCase() + value.slice(1) }))} /><Choice label={tr("Group agents")} value={group} onChange={setGroup} options={[{ value: 'workspace', label: tr("Workspace") }, { value: 'state', label: tr("State") }, { value: 'date', label: tr("Date") }, { value: 'none', label: tr("Ungrouped") }]} /></div>
        <div className="agent-scope"><button aria-pressed={scope} onClick={() => setScope(!scope)}>{scope ? tr("This workspace") : tr("All workspaces")}</button></div>
        <div className="section-scroll agent-list">{[...groups].map(([key, section]) => <div key={key}>
          {group !== 'none' && <button className="sidebar-group" aria-expanded={!collapsed.has(`agents:${key}`)} onClick={() => toggle(`agents:${key}`)}>{collapsed.has(`agents:${key}`) ? <ChevronRight size={12} /> : <ChevronDown size={12} />}{section.host && <span className="host-label">{section.host}</span>}<span>{section.label}</span><small>{section.rows.length}</small></button>}
          {!collapsed.has(`agents:${key}`) && section.rows.map(({ workspace, pane }) => <div className={`agent-row ${workspace.active && pane.focused ? 'active' : ''}`} key={pane.target ? targetKey(pane.target) : `${workspace.id}:${pane.id}`}>
            <button className="agent-main" onClick={() => { if (pane.target) void focusTarget(pane.target).catch(reportError); }} disabled={!pane.target} title={[text(pane.raw.agent_session_title),pane.agentName || pane.agent,pane.cwd || workspace.cwd,pane.agentStatus].filter(Boolean).join('\n')}>
              <StatusDot state={pane.agentStatus} /><span className="row-copy"><strong>{agentName(pane)}</strong>{layout.agent_status_names === true && !layout.agent_title_first && <small className="agent-state">{tr(pane.agentStatus)}</small>}{agentDetail(pane,workspace) && <small>{agentDetail(pane,workspace)}</small>}</span>
            </button><IconButton label={`Details for ${pane.title}`} onClick={() => onAgent(pane)}><MoreHorizontal size={14} /></IconButton>
          </div>)}
        </div>)}{!agents.length && <p className="sidebar-empty">{tr("No matching agents")}</p>}</div>
      </section>
    </div>
    <nav className="sidebar-bottom" aria-label={tr("Management")}><button className="footer-settings" onClick={() => setTool('settings')}><Settings2 size={15} />{tr("Settings")}</button><IconButton label={tr("Mission Control")} onClick={() => setTool('mission')}><Terminal size={15} /></IconButton><IconButton label={tr("Orchestration")} onClick={() => setTool('orch')}><Workflow size={15} /></IconButton><button className="footer-add" disabled={Boolean(remoteHost)} onClick={() => openDialog({ title: tr("Connect registered host"), submit: tr("Connect host"), fields: [{ key: 'alias', label: tr("SSH alias"), required: true }], run: value => window.luvus.sessions('host-add', String(value.alias), snapshot.generation) })}><Plus size={15} />{tr("Add machine")}</button></nav>
  </aside>;
}
