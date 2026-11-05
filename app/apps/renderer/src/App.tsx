import { tr } from './i18n';
import { useEffect, useRef, useState, type CSSProperties } from 'react';
import { ArrowLeft, Bot, Command, FileDiff, Files, FolderOpen, GitBranch, Globe, LayoutDashboard, PanelRightClose, Plus, Search, Settings2, Terminal, Workflow, Wrench, X, Package, Columns2 } from 'lucide-react';
import { clearError, clearNotice, connect, focusTarget, mutate, refreshSnapshot, reportError, request, useCore } from './bridge';
import { ActionDialog, openWorkspaceDialog, topologyDialog, splitPane, validateContext, type DialogSpec } from './Actions';
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
import { unpackColor } from '@luvus/terminal-view';
import { rememberBrowser } from './BrowserView';
import { useAppearance } from './appearance';
import { array, record, targetKey, text, type Pane } from './model';
import { NotificationInbox } from './NotificationInbox';
import { AsyncButton, Empty, ErrorBox, IconButton, Loading } from './ui';

const toolLabels: Record<Tool, string> = { files: 'Files', diff: 'Diff review', agents: 'Agent details', git: 'Source control', mission: 'Mission Control', orch: 'Orchestration', modules: 'Modules', settings: 'Settings', tools: 'Structured tools', connection: 'Core connection', search: 'Global finder' };
const largeTools = new Set<Tool>(['mission', 'orch', 'settings', 'tools', 'connection']);
function projectionLabel(state: unknown): string {
  if (state === 'upgrade_required') return tr('Owner upgrade required');
  if (state === 'disconnected') return 'SSH disconnected';
  if (state === 'switching') return 'Switching workspace…';
  if (state === 'unavailable') return 'Remote workspace unavailable';
  return 'Connecting to workspace…';
}
export function App() {
  const core = useCore(), snapshot = core.snapshot;
  const appearance = useAppearance();
  const creatingTab = useRef(false), creatingBrowser = useRef(false);
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
  const newTab = async () => {
    if (!snapshot || !workspace || workspace.host || creatingTab.current) return;
    creatingTab.current = true;
    try { validateContext(snapshot, workspace, undefined, undefined, true); await mutate('tab.new', {}, snapshot.generation); setTool(null); }
    finally { creatingTab.current = false; }
  };
  useEffect(() => {
    const palette = record(snapshot?.raw.terminal_palette);
    document.documentElement.style.setProperty('--terminal-fg', unpackColor(Number(palette.foreground), '#f1f3f7'));
    document.documentElement.style.setProperty('--terminal-bg', unpackColor(Number(palette.background), '#111214'));
  }, [snapshot?.raw.terminal_palette, appearance.mode]);
  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      if (!event.metaKey || event.altKey || event.ctrlKey || event.isComposing || document.querySelector('dialog[open]')) return;
      if (event.key.toLowerCase() === 'k') { event.preventDefault(); setPalette(true); }
      else if (event.key.toLowerCase() === 't' && snapshot && workspace) { event.preventDefault(); event.stopImmediatePropagation(); void newTab().catch(reportError); }
      else if (event.key.toLowerCase() === 'p') { event.preventDefault(); setTool('search'); }
      else if (event.key === ',') { event.preventDefault(); setTool('settings'); }
    };
    window.addEventListener('keydown', handler, true); return () => window.removeEventListener('keydown', handler, true);
  }, [snapshot, workspace]);
  useEffect(() => { setDialog(null); setAgentKey(null); }, [snapshot?.generation]);

  const commands: PaletteCommand[] = [
    { id: 'workspace-open', label: tr("Open workspace…"), detail: tr("Choose a directory"), run: () => setDialog(openWorkspaceDialog()) },
    ...Object.entries(toolLabels).map(([id, label]) => ({ id, label:tr(label), detail: tr("Desktop view"), run: () => setTool(id as Tool) })),
    { id: 'commander', label: tr("Toggle Commander"), detail: tr("Send a prompt to exact agent terminals"), run: () => setCommander((value) => !value) },
    { id: 'refresh', label: tr("Refresh desktop snapshot"), run: refreshSnapshot },
    ...(snapshot?.workspaces.map((w) => ({ id: `workspace:${w.id}`, label: w.name, detail: w.cwd, run: () => mutate('workspace.focus', { workspace_id: w.id }, snapshot.generation) })) ?? []),
    ...(snapshot?.workspaces.flatMap((w) => w.tabs.flatMap((t) => t.panes.flatMap((p) => p.target ? [{ id: `pane:${targetKey(p.target)}`, label: p.title, detail: `${w.name} › ${t.name || `Tab ${t.index1}`}`, run: () => focusTarget(p.target!) }] : []))) ?? []),
  ];
  if (snapshot && workspace && !workspace.host) commands.push({ id: 'tab-new', label: tr("New terminal tab"), shortcut: '⌘T', run: newTab });
  if (snapshot && workspace && !workspace.host && tab) {
    commands.push({ id: 'tab-rename', label: tr("Rename current tab…"), run: () => setDialog(topologyDialog('tab-rename', snapshot, workspace, tab)) });
    commands.push({ id: 'tab-close', label: tr("Close current tab…"), run: () => setDialog(topologyDialog('tab-close', snapshot, workspace, tab)) });
    for (const direction of ['left', 'right']) commands.push({ id: `tab-move-${direction}`, label: `Move current tab ${direction}`, run: () => mutate('tab.move', { tab_id: tab.id, direction }, snapshot.generation) });
  }
  if (snapshot && workspace && !workspace.host && tab && focused?.target) {
    for (const forward of [false, true]) commands.push({ id:forward ? 'focus-forward' : 'focus-back', label:forward ? tr("Focus forward") : tr("Focus back"),
      run:() => request({type:'focus_history', id:crypto.randomUUID(), target:focused.target!, forward}) });
    commands.push({ id: 'pane-split', label: tr("Split pane"), run: () => void splitPane(snapshot, workspace, tab, focused).catch(reportError) });
    commands.push({ id: 'agent-start', label: tr("Start an agent…"), run: () => setDialog(topologyDialog('agent-start', snapshot, workspace, tab, focused)) });
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
      case 'files': return workspace ? <FilesPanel key={key} {...props} workspace={workspace} /> : <Empty title={tr("Open a workspace to browse files")} />;
      case 'git': return workspace ? <GitPanel key={key} {...props} workspace={workspace} /> : <Empty title={tr("Open a Git workspace")} />;
      case 'diff': return workspace ? <DiffPanel key={key} {...props} workspace={workspace} /> : <Empty title={tr("Open a workspace to review changes")} />;
    }
  };
  const fullTool = snapshot && tool && largeTools.has(tool) ? tool : null;
  return <div ref={root} className="desktop-shell" style={{ '--sidebar-width': `${sidebarWidth}px`, '--tools-width': `${toolsWidth}px` } as CSSProperties}>
    {snapshot ? <Sidebar snapshot={snapshot} openDialog={setDialog} setTool={setTool} onAgent={inspectAgent} onPalette={() => setPalette(true)} /> : <aside className="sidebar"><div className="sidebar-titlebar"><span className="app-wordmark">Luvus</span></div><div className="sidebar-empty">{core.error ? tr("Disconnected") : tr("Connecting to the selected core…")}</div></aside>}
    <div className="sidebar-resizer" role="separator" aria-label={tr("Sidebar width")} aria-orientation="vertical" tabIndex={0} aria-valuemin={208} aria-valuemax={400} aria-valuenow={sidebarWidth} onKeyDown={(event) => { if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') { event.preventDefault(); setSidebarWidth((value) => Math.max(208, Math.min(400, value + (event.key === 'ArrowLeft' ? -8 : 8)))); } }} onPointerDown={(event) => event.currentTarget.setPointerCapture(event.pointerId)} onPointerMove={(event) => { if (event.currentTarget.hasPointerCapture(event.pointerId)) setSidebarWidth(Math.max(208, Math.min(400, event.clientX - (root.current?.getBoundingClientRect().left ?? 0)))); }} onPointerUp={(event) => event.currentTarget.releasePointerCapture(event.pointerId)} />
    <main className="main-shell"><header className="topbar"><div className="workspace-context"><FolderOpen size={16} /><strong>{workspace?.name ?? tr("Luvus desktop")}</strong>{workspace?.branch && <span className="branch"><GitBranch size={12} />{workspace.branch}</span>}{workspace?.host && <span className="badge">{workspace.host}</span>}</div><div className="toolbar no-drag">
      <IconButton label={tr("Command palette (Command K)")} onClick={() => setPalette(true)}><Command size={16} /></IconButton>
      <IconButton label={tr("Global finder (Command P)")} onClick={() => setTool(tool === 'search' ? null : 'search')}><Search size={16} /></IconButton>
      <IconButton label={tr("New browser tab")} disabled={!focused?.target || Boolean(workspace?.host)} onClick={() => { if (creatingBrowser.current || !focused?.target || !snapshot || !workspace) return;
        const target = focused.target; creatingBrowser.current = true;
        void (async () => {
        validateContext(snapshot, workspace, tab, focused, true);
        if (!array(snapshot.raw.desktop_features).includes('browser_tab')) throw new Error(tr("This running core needs an update to create browser tabs from the desktop."));
        const result = record(await request({ type: 'browser_tab', id: crypto.randomUUID(), target, url: 'about:blank' }).catch(error => {
          if (String(error).includes('invalid browser URL')) throw new Error(tr("This running core needs an update to create browser tabs from the desktop."));
          throw error;
        }));
        rememberBrowser(snapshot.generation, text(result.pane)); await refreshSnapshot(); setTool(null);
      })().catch(reportError).finally(() => { creatingBrowser.current = false; }); }}><Globe size={16} /></IconButton>
      <IconButton label={tr("Toggle Commander")} aria-pressed={commander} onClick={() => setCommander(!commander)}><Bot size={16} /></IconButton>
      <IconButton label={tr("Split active pane")} disabled={!focused?.target || Boolean(workspace?.host) || !snapshot || !workspace || !tab} onClick={() => { if (snapshot && workspace && !workspace.host && tab && focused) void splitPane(snapshot, workspace, tab, focused).catch(reportError); }}><Columns2 size={16} /></IconButton>
    </div></header>
    <div className="tabbar" role="tablist" aria-label={tr("Workspace tabs")}>{workspace?.tabs.map((item) => <div className={`tab-item ${item.active && !fullTool ? 'selected' : ''}`} key={item.id || item.index1}><button role="tab" aria-selected={item.active && !fullTool} onClick={() => { setTool(null); if (workspace.host) { const focus = item.panes.find(pane => pane.id === item.focus)?.target; if (focus) void focusTarget(focus).catch(reportError); } else void mutate('tab.focus', { tab_id: item.id }, snapshot?.generation).catch(reportError); }} onDoubleClick={() => { if (snapshot && !workspace.host) setDialog(topologyDialog('tab-rename', snapshot, workspace, item)); }}>{item.panes.length === 1 && item.panes[0]?.kind === 'browser' ? <Globe size={13} /> : item.kind === 'git' ? <GitBranch size={13} /> : item.kind === 'mission_control' ? <LayoutDashboard size={13} /> : item.kind === 'orchestration' ? <Workflow size={13} /> : <Terminal size={13} />}<span>{(item.panes.length === 1 && item.panes[0]?.kind === 'browser' ? core.browserTitles[item.panes[0].id] : '') || item.name || String(item.index1)}</span></button><IconButton disabled={Boolean(workspace.host)} label={`Close ${item.name || `tab ${item.index1}`}`} onClick={() => { if (snapshot) setDialog(topologyDialog('tab-close', snapshot, workspace, item)); }}><X size={12} /></IconButton></div>)}<IconButton label={tr("New tab")} disabled={!snapshot || !workspace || Boolean(workspace.host)} onClick={() => { if (snapshot && workspace) void newTab().catch(reportError); }}><Plus size={15} /></IconButton></div>
    {core.error && <div className="global-error"><ErrorBox error={core.error} />{core.ready && <IconButton label={tr("Dismiss error")} onClick={clearError}><X size={14} /></IconButton>}</div>}

    <div className="workbench"><div className="central-stack"><div className="central-content">
      {fullTool ? <div className="full-tool"><div className="full-tool-nav"><button className="text-button" onClick={() => setTool(null)}><ArrowLeft size={14} />{tr("Back to terminals")}</button></div>{renderTool(fullTool)}</div> : !snapshot ? <Empty title={core.error ? tr("Unable to connect to the core") : tr("Connecting to Luvus")} action={core.error && <AsyncButton className="button primary" run={() => window.luvus.reconnect()}>{tr("Reconnect to session")}</AsyncButton>}>{core.error ? tr("The app will not restart your server or fall back to a full TUI.") : tr("Waiting for a compatible desktop projection…")}</Empty> : !workspace ? <Empty title={tr("Your workspace starts here")} action={<button className="button primary" onClick={() => setDialog(openWorkspaceDialog())}><FolderOpen size={16} />{tr("Open workspace")}</button>}>{tr("Open a project directory. The core owns your shells, agents and layout; this window is just a client.")}</Empty> : !workspace.guiAvailable ? <Empty title={tr(projectionLabel(workspace.raw.projection_status))}>{workspace.host}</Empty> : !tab ? <Empty title={tr("No active tab")} /> : tab.kind === 'git' ? renderTool('git') : tab.kind === 'mission_control' ? renderTool('mission') : tab.kind === 'orchestration' ? renderTool('orch') : <WorkspaceArea snapshot={snapshot} workspace={workspace} tab={tab} openDialog={setDialog} onAgent={inspectAgent} />}
    </div>{snapshot && <Commander key={snapshot.generation} snapshot={snapshot} visible={commander} openDialog={setDialog} onClose={() => setCommander(false)} />}</div>
    {snapshot && tool && !fullTool && <aside className="tools-panel" aria-label={tr(toolLabels[tool])}><div className="tools-resizer" role="separator" aria-label={tr("Tool panel width")} aria-orientation="vertical" tabIndex={0} aria-valuenow={toolsWidth} aria-valuemin={280} aria-valuemax={680} onKeyDown={(event) => { if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') { event.preventDefault(); setToolsWidth((value) => Math.max(280, Math.min(680, value + (event.key === 'ArrowLeft' ? 8 : -8)))); } }} onPointerDown={(event) => event.currentTarget.setPointerCapture(event.pointerId)} onPointerMove={(event) => { if (event.currentTarget.hasPointerCapture(event.pointerId)) setToolsWidth(Math.max(280, Math.min(680, (root.current?.getBoundingClientRect().right ?? window.innerWidth) - event.clientX - 40))); }} onPointerUp={(event) => event.currentTarget.releasePointerCapture(event.pointerId)} /><div className="tools-close"><IconButton label={tr("Close tools panel")} onClick={() => setTool(null)}><PanelRightClose size={15} /></IconButton></div>{renderTool(tool)}</aside>}
    <nav className="tool-rail" aria-label={tr("Tools")}>{([{ id: 'files', icon: Files }, { id: 'diff', icon: FileDiff }, { id: 'git', icon: GitBranch }, { id: 'agents', icon: Bot }, { id: 'modules', icon: Package }, { id: 'tools', icon: Wrench }] as const).map(({ id, icon: Icon }) => <IconButton key={id} label={tr(toolLabels[id])} aria-pressed={tool === id} onClick={() => setTool(tool === id ? null : id)}><Icon size={17} /></IconButton>)}<div className="rail-spacer" /><IconButton label={tr("Settings")} aria-pressed={tool === 'settings'} onClick={() => setTool(tool === 'settings' ? null : 'settings')}><Settings2 size={17} /></IconButton></nav>
    </div><footer className="statusbar"><span className={`connection-dot ${core.ready ? 'online' : ''}`} /><span>{core.ready ? tr("Connected") : tr("Disconnected")}</span><button onClick={() => setTool('connection')}>{snapshot?.session ?? tr("Core session")}</button>{core.notice ? <button className="status-notice" role="status" title={core.notice} onClick={clearNotice}>{core.notice}</button> : snapshot && <ProjectedBars snapshot={snapshot} />}<span className="status-path" title={focused?.cwd || workspace?.cwd}>{focused?.cwd || workspace?.cwd}</span>{snapshot && <span className="status-generation" title={snapshot.generation}>{tr("generation")} {snapshot.generation.slice(0, 8)}</span>}{snapshot && <NotificationInbox key={snapshot.generation} snapshot={snapshot} />}</footer>
    </main>
    {dialog && <ActionDialog key={dialog.title} spec={dialog} onClose={() => setDialog(null)} />}
    {palette && <CommandPalette commands={commands} onClose={() => setPalette(false)} />}
  </div>;
}
