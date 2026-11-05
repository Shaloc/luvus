import { useState } from 'react';
import { Bot, ChevronDown, Folder, GitBranch, MoreHorizontal, Pin, Plus, Search, Server, Settings2, Terminal, Workflow } from 'lucide-react';
import { focusTarget, mutate, reportError, useCore } from './bridge';
import { openWorkspaceDialog, topologyDialog, type OpenDialog } from './Actions';
import { targetKey, type Pane, type Snapshot } from './model';
import { IconButton, StatusDot } from './ui';

export type Tool = 'files' | 'diff' | 'agents' | 'git' | 'mission' | 'orch' | 'modules' | 'settings' | 'tools' | 'connection' | 'search';
interface Props { snapshot: Snapshot; openDialog: OpenDialog; setTool: (tool: Tool) => void; onAgent: (pane: Pane) => void; onPalette: () => void }
export function Sidebar({ snapshot, openDialog, setTool, onAgent, onPalette }: Props) {
  const [filter, setFilter] = useState('all'), [scope, setScope] = useState(false);
  const { remoteHost } = useCore();
  const workspaces = [...snapshot.workspaces].sort((a, b) => a.order - b.order);
  const agents = workspaces.filter((w) => !scope || w.active).flatMap((workspace) => workspace.tabs.flatMap((tab) => tab.panes.filter((pane) => pane.isAgent).map((pane) => ({ workspace, tab, pane })))).filter(({ pane }) => filter === 'all' || pane.agentStatus === filter);
  return <aside className="sidebar" aria-label="Workspaces and agents">
    <div className="sidebar-titlebar"><span className="app-wordmark">Luvus</span></div>
    <button className="session-selector" onClick={() => setTool('connection')}><Server size={16} /><span><strong>{snapshot.session}</strong><small title={remoteHost}>{remoteHost ? `SSH: ${remoteHost}` : 'Local session'}</small></span><ChevronDown size={14} /></button>
    <button className="sidebar-search" onClick={onPalette}><Search size={14} /><span>Find a command…</span><kbd>⌘ K</kbd></button>
    <div className="sidebar-scroll">
      <div className="sidebar-section-heading"><span>Workspaces</span><IconButton label="Open workspace" onClick={() => openDialog(openWorkspaceDialog())}><Plus size={14} /></IconButton></div>
      <nav aria-label="Workspaces">{workspaces.map((workspace) => <div className={`workspace-row ${workspace.active ? 'active' : ''} ${workspace.parentId ? 'worktree-row' : ''}`} key={workspace.id}>
        <button className="workspace-main" onClick={() => { void mutate('workspace.focus', { workspace_id: workspace.id }, snapshot.generation).catch(reportError); }} title={workspace.cwd} aria-current={workspace.active ? 'page' : undefined}>
          {workspace.parentId ? <GitBranch size={15} /> : <Folder size={15} />}<span><strong>{workspace.name}</strong>{workspace.host && <small>{workspace.host}</small>}</span>{workspace.pinned && <Pin size={11} />}
        </button>
        <details className="dropdown"><summary className="icon-button" aria-label={`Actions for ${workspace.name}`}><MoreHorizontal size={14} /></summary><div className="dropdown-menu">
          <button onClick={() => openDialog(topologyDialog('workspace-rename', snapshot, workspace))}>Rename…</button>
          <button onClick={() => { void mutate('workspace.pin', { workspace_id: workspace.id, pinned: !workspace.pinned }, snapshot.generation).catch(reportError); }}>{workspace.pinned ? 'Unpin' : 'Pin workspace'}</button>
          <button className="destructive-text" onClick={() => openDialog(topologyDialog('workspace-close', snapshot, workspace))}>Close workspace…</button>
        </div></details>
      </div>)}</nav>
      {!workspaces.length && <p className="sidebar-empty">Open a directory to start a workspace.</p>}
      <div className="sidebar-section-heading agent-section-heading"><span>Agents <span className="count">{agents.length}</span></span><IconButton label="Agent details and history" onClick={() => setTool('agents')}><Bot size={14} /></IconButton></div>
      <div className="agent-filters"><select aria-label="Filter agents by state" value={filter} onChange={(event) => setFilter(event.target.value)}><option value="all">All states</option>{Array.from(new Set(workspaces.flatMap((w) => w.tabs.flatMap((t) => t.panes.filter((p) => p.isAgent).map((p) => p.agentStatus))))).filter(Boolean).map((value) => <option key={value}>{value}</option>)}</select><button aria-pressed={scope} onClick={() => setScope(!scope)} title="Limit agents to active workspace">{scope ? 'This workspace' : 'All workspaces'}</button></div>
      <div className="agent-list">{agents.map(({ workspace, pane }) => <div className={`agent-row ${pane.focused ? 'active' : ''}`} key={pane.target ? targetKey(pane.target) : `${workspace.id}:${pane.id}`}>
        <button className="agent-main" onClick={() => { if (pane.target) void focusTarget(pane.target).catch(reportError); }} disabled={!pane.target}><StatusDot state={pane.agentStatus} /><span><strong>{pane.title}</strong><small>{pane.agent} · {workspace.name}</small></span></button>
        <IconButton label={`Details for ${pane.title}`} onClick={() => onAgent(pane)}><MoreHorizontal size={14} /></IconButton>
      </div>)}</div>
      {!agents.length && <p className="sidebar-empty">No matching active agents.<br />Start an agent from a terminal pane.</p>}
    </div>
    <nav className="sidebar-bottom" aria-label="Management"><button onClick={() => setTool('mission')}><Terminal size={15} />Mission Control</button><button onClick={() => setTool('orch')}><Workflow size={15} />Orchestration</button><button onClick={() => setTool('settings')}><Settings2 size={15} />Settings</button></nav>
  </aside>;
}
