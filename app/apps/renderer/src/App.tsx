import { useEffect, useRef, useState, type CSSProperties } from 'react';
import { ArrowLeft, Bot, Command, FileDiff, Files, FolderOpen, GitBranch, Globe, LayoutDashboard, PanelRightClose, Plus, Search, Settings2, Terminal, Workflow, Wrench, X, Package, Columns2 } from 'lucide-react';
import { clearError, clearNotice, connect, focusTarget, mutate, refreshSnapshot, reportError, request, useCore } from './bridge';
import { ActionDialog, openWorkspaceDialog, topologyDialog, type DialogSpec } from './Actions';
import { CommandPalette, type PaletteCommand } from './CommandPalette';
import { Commander } from './Commander';
import { AgentPanel, MissionPanel, OrchPanel } from './ManagementPanels';
import { DiffPanel, FilesPanel, GitPanel } from './RepositoryPanels';
import { ConnectionPanel, ModulesPanel, SettingsPanel } from './SettingsPanels';
import { SearchPanel } from './SearchPanel';
import { Sidebar, type Tool } from './Sidebar';
import { ToolsPanel } from './ToolsPanel';
import { WorkspaceArea } from './WorkspaceArea';
import { ProjectedBars } from './ProjectedWidgets';
import { safeLink } from '@luvus/terminal-view';
import { targetKey, text, type Pane } from './model';
import { Empty, ErrorBox, IconButton, Loading } from './ui';

const toolLabels: Record<Tool, string> = { files: 'Files', diff: 'Diff review', agents: 'Agent details', git: 'Source control', mission: 'Mission Control', orch: 'Orchestration', modules: 'Modules', settings: 'Settings', tools: 'Structured tools', connection: 'Core connection', search: 'Global finder' };
const largeTools = new Set<Tool>(['mission', 'orch', 'settings', 'tools', 'connection']);
function projectionLabel(state: unknown): string {
  if (state === 'upgrade_required') return 'Owner upgrade required';
  if (state === 'disconnected') return 'SSH disconnected';
  if (state === 'switching') return 'Switching workspace…';
  if (state === 'unavailable') return 'Remote workspace unavailable';
  return 'Connecting to workspace…';
}
export function App() {
  const core = useCore(), snapshot = core.snapshot;
  const [dialog, setDialog] = useState<DialogSpec | null>(null), [palette, setPalette] = useState(false), [commander, setCommander] = useState(false);
  const [tool, setTool] = useState<Tool | null>(null), [agentKey, setAgentKey] = useState<string | null>(null);
  const [sidebarWidth, setSidebarWidth] = useState(256), [toolsWidth, setToolsWidth] = useState(360);
  const root = useRef<HTMLDivElement>(null);
  const workspace = snapshot?.workspaces.find((item) => item.active);
  const tab = workspace?.tabs.find((item) => item.active), focused = tab?.panes.find((pane) => pane.id === tab.focus) ?? tab?.panes.find((pane) => pane.focused);
  const selectedAgent = snapshot?.workspaces.flatMap((w) => w.tabs.flatMap((t) => t.panes)).find((p) => p.target && targetKey(p.target) === agentKey) ?? null;
  const agentWorkspace = snapshot?.workspaces.find((w) => w.tabs.some((t) => t.panes.includes(selectedAgent!)));
  const inspectAgent = (pane: Pane) => { setAgentKey(pane.target ? targetKey(pane.target) : null); setTool('agents'); };
  useEffect(() => { void connect(); }, []);
  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      if (!event.metaKey || event.altKey || event.ctrlKey || event.isComposing || document.querySelector('dialog[open]')) return;
      if (event.key.toLowerCase() === 'k') { event.preventDefault(); setPalette(true); }
      else if (event.key.toLowerCase() === 't' && snapshot && workspace) { event.preventDefault(); setDialog(topologyDialog('tab-new', snapshot, workspace)); }
      else if (event.key.toLowerCase() === 'p') { event.preventDefault(); setTool('search'); }
      else if (event.key === ',') { event.preventDefault(); setTool('settings'); }
    };
    window.addEventListener('keydown', handler); return () => window.removeEventListener('keydown', handler);
  }, [snapshot, workspace]);
  useEffect(() => { setDialog(null); setAgentKey(null); }, [snapshot?.generation]);

  const commands: PaletteCommand[] = [
    { id: 'workspace-open', label: 'Open workspace…', detail: 'Choose a directory', run: () => setDialog(openWorkspaceDialog()) },
    ...Object.entries(toolLabels).map(([id, label]) => ({ id, label, detail: 'Desktop view', run: () => setTool(id as Tool) })),
    { id: 'commander', label: 'Toggle Commander', detail: 'Send a prompt to exact agent terminals', run: () => setCommander((value) => !value) },
    { id: 'refresh', label: 'Refresh desktop snapshot', run: refreshSnapshot },
    ...(snapshot?.workspaces.map((w) => ({ id: `workspace:${w.id}`, label: w.name, detail: w.cwd, run: () => mutate('workspace.focus', { workspace_id: w.id }, snapshot.generation) })) ?? []),
    ...(snapshot?.workspaces.flatMap((w) => w.tabs.flatMap((t) => t.panes.flatMap((p) => p.target ? [{ id: `pane:${targetKey(p.target)}`, label: p.title, detail: `${w.name} › ${t.name || `Tab ${t.index1}`}`, run: () => focusTarget(p.target!) }] : []))) ?? []),
  ];
  if (snapshot && workspace && !workspace.host) commands.push({ id: 'tab-new', label: 'New terminal tab…', shortcut: '⌘T', run: () => setDialog(topologyDialog('tab-new', snapshot, workspace)) });
  if (snapshot && workspace && !workspace.host && tab) {
    commands.push({ id: 'tab-rename', label: 'Rename current tab…', run: () => setDialog(topologyDialog('tab-rename', snapshot, workspace, tab)) });
    commands.push({ id: 'tab-close', label: 'Close current tab…', run: () => setDialog(topologyDialog('tab-close', snapshot, workspace, tab)) });
    for (const direction of ['left', 'right']) commands.push({ id: `tab-move-${direction}`, label: `Move current tab ${direction}`, run: () => mutate('tab.move', { tab_id: tab.id, direction }, snapshot.generation) });
  }
  if (snapshot && workspace && !workspace.host && tab && focused?.target) {
    for (const forward of [false, true]) commands.push({ id:forward ? 'focus-forward' : 'focus-back', label:forward ? 'Focus forward' : 'Focus back',
      run:() => request({type:'focus_history', id:crypto.randomUUID(), target:focused.target!, forward}) });
    commands.push({ id: 'pane-split', label: 'Split pane…', run: () => setDialog(topologyDialog('pane-split', snapshot, workspace, tab, focused)) });
    commands.push({ id: 'agent-start', label: 'Start an agent…', run: () => setDialog(topologyDialog('agent-start', snapshot, workspace, tab, focused)) });
  }
  const renderTool = (selected: Tool) => {
    if (!snapshot) return <Loading />;
    const props = { snapshot, workspace, openDialog: setDialog };
    const key = `${snapshot.generation}:${workspace?.id ?? ''}:${selected}`;
    switch (selected) {
      case 'mission': return <MissionPanel key={key} {...props} />;
      case 'orch': return <OrchPanel key={key} {...props} />;
      case 'modules': return <ModulesPanel key={key} {...props} />;
      case 'settings': return <SettingsPanel key={key} {...props} />;
      case 'tools': return <ToolsPanel key={key} />;
      case 'connection': return <ConnectionPanel key={key} {...props} />;
      case 'search': return <SearchPanel key={key} snapshot={snapshot} />;
      case 'agents': return <AgentPanel key={key} {...props} workspace={agentWorkspace} pane={selectedAgent} />;
      case 'files': return workspace ? <FilesPanel key={key} {...props} workspace={workspace} /> : <Empty title="Open a workspace to browse files" />;
      case 'git': return workspace ? <GitPanel key={key} {...props} workspace={workspace} /> : <Empty title="Open a Git workspace" />;
      case 'diff': return workspace ? <DiffPanel key={key} {...props} workspace={workspace} /> : <Empty title="Open a workspace to review changes" />;
    }
  };
  const fullTool = tool && largeTools.has(tool) ? tool : null;
  return <div ref={root} className="desktop-shell" style={{ '--sidebar-width': `${sidebarWidth}px`, '--tools-width': `${toolsWidth}px` } as CSSProperties}>
    {snapshot ? <Sidebar snapshot={snapshot} openDialog={setDialog} setTool={setTool} onAgent={inspectAgent} onPalette={() => setPalette(true)} /> : <aside className="sidebar"><div className="sidebar-titlebar"><span className="app-wordmark">Luvus</span></div><div className="sidebar-empty">Connecting to the selected core…</div></aside>}
    <div className="sidebar-resizer" role="separator" aria-label="Sidebar width" aria-orientation="vertical" tabIndex={0} aria-valuemin={208} aria-valuemax={400} aria-valuenow={sidebarWidth} onKeyDown={(event) => { if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') { event.preventDefault(); setSidebarWidth((value) => Math.max(208, Math.min(400, value + (event.key === 'ArrowLeft' ? -8 : 8)))); } }} onPointerDown={(event) => event.currentTarget.setPointerCapture(event.pointerId)} onPointerMove={(event) => { if (event.currentTarget.hasPointerCapture(event.pointerId)) setSidebarWidth(Math.max(208, Math.min(400, event.clientX - (root.current?.getBoundingClientRect().left ?? 0)))); }} onPointerUp={(event) => event.currentTarget.releasePointerCapture(event.pointerId)} />
    <main className="main-shell"><header className="topbar"><div className="workspace-context"><FolderOpen size={16} /><strong>{workspace?.name ?? 'Luvus desktop'}</strong>{workspace?.branch && <span className="branch"><GitBranch size={12} />{workspace.branch}</span>}{workspace?.host && <span className="badge">{workspace.host}</span>}</div><div className="toolbar no-drag">
      <IconButton label="Command palette (Command K)" onClick={() => setPalette(true)}><Command size={16} /></IconButton>
      <IconButton label="Global finder (Command P)" onClick={() => setTool(tool === 'search' ? null : 'search')}><Search size={16} /></IconButton>
      <IconButton label="Open browser leaf" disabled={!focused?.target || Boolean(workspace?.host)} onClick={() => setDialog({ title: 'Open browser leaf', submit: 'Open browser', description: 'Creates a core-owned browser leaf. Open it to use this workspace’s localhost and proxy settings.', fields: [{ key: 'url', label: 'HTTP(S) URL', required: true }], run: (v) => { const url = safeLink(text(v.url)); if (!url || !focused?.target) throw new Error('Enter a valid HTTP(S) URL without credentials.'); return mutate('browser.panel', { pane: focused.id, url }, focused.target.generation); } })}><Globe size={16} /></IconButton>
      <IconButton label="Toggle Commander" aria-pressed={commander} onClick={() => setCommander(!commander)}><Bot size={16} /></IconButton>
      <IconButton label="Split active pane" disabled={!focused?.target || Boolean(workspace?.host) || !snapshot || !workspace || !tab} onClick={() => { if (snapshot && workspace && !workspace.host && tab && focused) setDialog(topologyDialog('pane-split', snapshot, workspace, tab, focused)); }}><Columns2 size={16} /></IconButton>
    </div></header>
    {core.error && <div className="global-error"><ErrorBox error={core.error} /><IconButton label="Dismiss error" onClick={clearError}><X size={14} /></IconButton></div>}
    {core.notice && <div className="global-notice" role="status"><span>{core.notice}</span><IconButton label="Dismiss notification" onClick={clearNotice}><X size={14} /></IconButton></div>}
    <div className="tabbar" role="tablist" aria-label="Workspace tabs">{workspace?.tabs.map((item) => <div className={`tab-item ${item.active && !fullTool ? 'selected' : ''}`} key={item.id || item.index1}><button role="tab" aria-selected={item.active && !fullTool} onClick={() => { setTool(null); if (workspace.host) { const focus = item.panes.find(pane => pane.id === item.focus)?.target; if (focus) void focusTarget(focus).catch(reportError); } else void mutate('tab.focus', { tab_id: item.id }, snapshot?.generation).catch(reportError); }} onDoubleClick={() => { if (snapshot && !workspace.host) setDialog(topologyDialog('tab-rename', snapshot, workspace, item)); }}>{item.kind === 'git' ? <GitBranch size={13} /> : item.kind === 'mission_control' ? <LayoutDashboard size={13} /> : item.kind === 'orchestration' ? <Workflow size={13} /> : <Terminal size={13} />}<span>{item.name || `Terminal ${item.index1}`}</span></button><IconButton disabled={Boolean(workspace.host)} label={`Close ${item.name || `tab ${item.index1}`}`} onClick={() => { if (snapshot) setDialog(topologyDialog('tab-close', snapshot, workspace, item)); }}><X size={12} /></IconButton></div>)}<IconButton label="New tab" disabled={!snapshot || !workspace || Boolean(workspace.host)} onClick={() => { if (snapshot && workspace) setDialog(topologyDialog('tab-new', snapshot, workspace)); }}><Plus size={15} /></IconButton></div>
    <div className="workbench"><div className="central-stack"><div className="central-content">
      {fullTool ? <div className="full-tool"><div className="full-tool-nav"><button className="text-button" onClick={() => setTool(null)}><ArrowLeft size={14} />Back to terminals</button></div>{renderTool(fullTool)}</div> : !snapshot ? <Empty title={core.error ? 'Unable to connect to the core' : 'Connecting to Luvus'}>{core.error ? 'The app will not restart your server or fall back to a full TUI.' : 'Waiting for a compatible desktop projection…'}</Empty> : !workspace ? <Empty title="Your workspace starts here" action={<button className="button primary" onClick={() => setDialog(openWorkspaceDialog())}><FolderOpen size={16} />Open workspace</button>}>Open a project directory. The core owns your shells, agents and layout; this window is just a client.</Empty> : !workspace.guiAvailable ? <Empty title={projectionLabel(workspace.raw.projection_status)}>{workspace.host}</Empty> : !tab ? <Empty title="No active tab" /> : tab.kind === 'git' ? renderTool('git') : tab.kind === 'mission_control' ? renderTool('mission') : tab.kind === 'orchestration' ? renderTool('orch') : <WorkspaceArea snapshot={snapshot} workspace={workspace} tab={tab} openDialog={setDialog} onAgent={inspectAgent} />}
    </div>{snapshot && <Commander key={snapshot.generation} snapshot={snapshot} visible={commander} openDialog={setDialog} onClose={() => setCommander(false)} />}</div>
    {tool && !fullTool && <aside className="tools-panel" aria-label={toolLabels[tool]}><div className="tools-resizer" role="separator" aria-label="Tool panel width" aria-orientation="vertical" tabIndex={0} aria-valuenow={toolsWidth} aria-valuemin={280} aria-valuemax={680} onKeyDown={(event) => { if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') { event.preventDefault(); setToolsWidth((value) => Math.max(280, Math.min(680, value + (event.key === 'ArrowLeft' ? 8 : -8)))); } }} onPointerDown={(event) => event.currentTarget.setPointerCapture(event.pointerId)} onPointerMove={(event) => { if (event.currentTarget.hasPointerCapture(event.pointerId)) setToolsWidth(Math.max(280, Math.min(680, (root.current?.getBoundingClientRect().right ?? window.innerWidth) - event.clientX - 40))); }} onPointerUp={(event) => event.currentTarget.releasePointerCapture(event.pointerId)} /><div className="tools-close"><IconButton label="Close tools panel" onClick={() => setTool(null)}><PanelRightClose size={15} /></IconButton></div>{renderTool(tool)}</aside>}
    <nav className="tool-rail" aria-label="Tools">{([{ id: 'files', icon: Files }, { id: 'diff', icon: FileDiff }, { id: 'git', icon: GitBranch }, { id: 'agents', icon: Bot }, { id: 'modules', icon: Package }, { id: 'tools', icon: Wrench }] as const).map(({ id, icon: Icon }) => <IconButton key={id} label={toolLabels[id]} aria-pressed={tool === id} onClick={() => setTool(tool === id ? null : id)}><Icon size={17} /></IconButton>)}<div className="rail-spacer" /><IconButton label="Settings" aria-pressed={tool === 'settings'} onClick={() => setTool(tool === 'settings' ? null : 'settings')}><Settings2 size={17} /></IconButton></nav>
    </div><footer className="statusbar"><span className={`connection-dot ${core.ready ? 'online' : ''}`} /><span>{core.ready ? 'Connected' : 'Disconnected'}</span><button onClick={() => setTool('connection')}>{snapshot?.session ?? 'Core session'}</button>{snapshot && <ProjectedBars snapshot={snapshot} />}<span className="status-path" title={focused?.cwd || workspace?.cwd}>{focused?.cwd || workspace?.cwd}</span>{snapshot && <span className="status-generation" title={snapshot.generation}>generation {snapshot.generation.slice(0, 8)}</span>}</footer>
    </main>
    {dialog && <ActionDialog key={dialog.title} spec={dialog} onClose={() => setDialog(null)} />}
    {palette && <CommandPalette commands={commands} onClose={() => setPalette(false)} />}
  </div>;
}
