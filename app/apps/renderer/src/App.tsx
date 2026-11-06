import { tr } from './i18n';
import { useEffect, useRef, useState, type CSSProperties } from 'react';
import { ArrowLeft, Bot, Command, FileDiff, Files, FolderOpen, GitBranch, Globe, LayoutDashboard, PanelLeftClose, PanelLeftOpen, PanelRightClose, PanelRightOpen, Plus, Search, Settings2, Terminal, Workflow, Wrench, X, Package, Columns2 } from 'lucide-react';
import { clearError, clearNotice, connect, focusTarget, mutate, notify, refreshSnapshot, reportError, request, useCore } from './bridge';
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
import { rememberBrowser } from './BrowserView';
import { useAppearance, useTerminalPalette } from './appearance';
import { array, record, targetKey, text, type Layout, type Pane, type Tab } from './model';
import { NotificationInbox } from './NotificationInbox';
import { useKeyboardShortcuts } from './useKeyboardShortcuts';
import { ShortcutHelp } from './ShortcutHelp';
import { AsyncButton, Empty, ErrorBox, IconButton, Loading } from './ui';

const toolLabels: Record<Tool, string> = { files: 'Files', diff: 'Diff review', agents: 'Agent details', git: 'Source control', mission: 'Mission Control', orch: 'Orchestration', modules: 'Modules', settings: 'Settings', tools: 'Structured tools', connection: 'Core connection', schedules: 'Scheduled tasks', search: 'Global finder' };
const largeTools = new Set<Tool>(['mission', 'orch', 'settings', 'tools', 'connection', 'schedules']);
function projectionLabel(state: unknown): string {
  if (state === 'upgrade_required') return tr('Owner upgrade required');
  if (state === 'disconnected') return 'SSH disconnected';
  if (state === 'switching') return 'Switching workspace…';
  if (state === 'unavailable') return 'Remote workspace unavailable';
  return 'Connecting to workspace…';
}
export function App() {
  const core = useCore(), snapshot = core.snapshot;
  const appearance = useAppearance(), terminalPalette = useTerminalPalette(appearance.mode, snapshot?.raw.terminal_palette);
  const [leftHidden, setLeftHidden] = useState(() => localStorage.getItem('luvus.desktop.hideLeft') === 'true');
  const [rightHidden, setRightHidden] = useState(() => localStorage.getItem('luvus.desktop.hideRight') === 'true');
  useEffect(() => { localStorage.setItem('luvus.desktop.hideLeft', String(leftHidden)); }, [leftHidden]);
  useEffect(() => { localStorage.setItem('luvus.desktop.hideRight', String(rightHidden)); }, [rightHidden]);
  const creatingTab = useRef(false), creatingBrowser = useRef(false);
  const [dialog, setDialog] = useState<DialogSpec | null>(null), [palette, setPalette] = useState(false), [commander, setCommander] = useState(false);
  const [keyHelp,setKeyHelp] = useState(false);
  const [tool, selectTool] = useState<Tool | null>(null), [agentKey, setAgentKey] = useState<string | null>(null);
  const [sidebarWidth, setSidebarWidth] = useState(256), [toolsWidth, setToolsWidth] = useState(360);
  const root = useRef<HTMLDivElement>(null);
  const workspace = snapshot?.workspaces.find((item) => item.active);
  const tab = workspace?.tabs.find((item) => item.active), focused = tab?.panes.find((pane) => pane.id === tab.focus) ?? tab?.panes.find((pane) => pane.focused);
  const selectedAgent = snapshot?.workspaces.flatMap((w) => w.tabs.flatMap((t) => t.panes)).find((p) => p.target && targetKey(p.target) === agentKey) ?? null;
  const agentWorkspace = snapshot?.workspaces.find((w) => w.tabs.some((t) => t.panes.includes(selectedAgent!)));
  const setTool = (next: Tool | null) => {
    selectTool(next);
    if (next && !largeTools.has(next)) setRightHidden(false);
  };
  const inspectAgent = (pane: Pane) => { setAgentKey(pane.target ? targetKey(pane.target) : null); setTool('agents'); };
  useEffect(() => { void connect(); }, []);
  const newTab = async () => {
    if (!snapshot || !workspace || workspace.host || creatingTab.current) return;
    creatingTab.current = true;
    try { validateContext(snapshot, workspace, undefined, undefined, true); await mutate('tab.new', {}, snapshot.generation); setTool(null); }
    finally { creatingTab.current = false; }
  };
  const closeTab = (selected: Tab) => {
    if (!snapshot || !workspace || workspace.host) return;
    const action = topologyDialog('tab-close', snapshot, workspace, selected);
    if (appearance.confirmTabClose) setDialog(action);
    else void action.run({}).catch(reportError);
  };
  const selectTab = async (selected: Tab) => {
    if (!snapshot || !workspace) return;
    validateContext(snapshot,workspace,selected,undefined,true);
    setTool(null);
    if (workspace.host) {
      const target=selected.panes.find(p=>p.id===selected.focus)?.target;
      if (target) await focusTarget(target);
    } else await mutate('tab.focus',{tab_id:selected.id},snapshot.generation);
  };
  const shortcutRoute = [snapshot?.generation,workspace?.id,tab?.id,focused?.id,focused?.target?.terminal_id,tool,Boolean(dialog),palette,keyHelp,commander].join(':');
  const prefix = useKeyboardShortcuts(snapshot,shortcutRoute,async command=>{
    if (!snapshot) return;
    if (command.startsWith('tab:') || command==='next_tab' || command==='prev_tab') {
      if (!workspace?.tabs.length) return;
      const position = command.startsWith('tab:') ? Number(command.slice(4))-1 :
        (workspace.tabs.findIndex(t=>t.active)+(command==='next_tab'?1:-1)+workspace.tabs.length)%workspace.tabs.length;
      const selected=workspace.tabs[position]; if(selected) await selectTab(selected);return;
    }
    if (command==='help_keys') {setKeyHelp(true);return;}
    if (command==='toggle_sidebar') {const hide=!(leftHidden && rightHidden);setLeftHidden(hide);setRightHidden(hide);return;}
    if (command==='toggle_right_sidebar') {setRightHidden(!rightHidden);return;}
    if (command==='open_commander') {setCommander(true);return;}
    if (command==='switcher') {setPalette(true);return;}
    if (command==='detach') {window.close();return;}
    const tools:Record<string,Tool>={open_git:'git',open_diff:'diff',open_mission:'mission',open_board:'orch',open_settings:'settings',toggle_files:'files',search:'search'};
    if (tools[command]) {setTool(tools[command]!);return;}
    if (command==='new_node') {setDialog(openWorkspaceDialog());return;}
    if (['open_sessions','focus_workspaces','toggle_agents','toggle_agent_scope'].includes(command)) {
      setLeftHidden(false);
      // Invoke the same controls used by pointer interaction after unhiding them.
      requestAnimationFrame(()=>{
        if(command==='open_sessions') {document.querySelector<HTMLButtonElement>('.session-selector')?.click();return;}
        if(command==='toggle_agent_scope') {document.querySelector<HTMLButtonElement>('.agent-scope button')?.click();return;}
        const section=command==='focus_workspaces'?'.workspace-section':'.agent-section';
        const disclosure=document.querySelector<HTMLButtonElement>(section+' .section-disclosure');
        if(disclosure?.getAttribute('aria-expanded')==='false') disclosure.click();
        requestAnimationFrame(()=>document.querySelector<HTMLButtonElement>(section+' .section-scroll button')?.focus());
      });return;
    }
    if (command.startsWith('jump_workspace_') || command==='next_node' || command==='prev_node') {
      const ordered=[...snapshot.workspaces].sort((a,b)=>a.order-b.order);
      const index=command.startsWith('jump_workspace_') ? Number(command.slice(15))-1 :
        (ordered.findIndex(w=>w.active)+(command==='next_node'?1:-1)+ordered.length)%ordered.length;
      const w=ordered[index];if(w){setTool(null);await mutate('workspace.focus',{workspace_id:w.id},snapshot.generation);}return;
    }
    if (command==='next_attention' || command==='next_done_agent') {
      const panes=snapshot.workspaces.flatMap(w=>w.tabs.flatMap(t=>t.panes)).filter(p=>p.target && p.isAgent && p.agentStatus===(command==='next_attention'?'blocked':'done'));
      const next=panes[(panes.findIndex(p=>p.target && focused?.target && targetKey(p.target)===targetKey(focused.target))+1)%panes.length];
      if(next?.target){setTool(null);await focusTarget(next.target);}return;
    }
    if (!workspace || !tab) return;
    validateContext(snapshot,workspace,tab,focused,true);
    if (command==='next_pane') {
      const next=tab.panes[(tab.panes.findIndex(p=>p.id===focused?.id)+1)%tab.panes.length];
      if(next?.target) await focusTarget(next.target);return;
    }
    if (command.startsWith('scroll:') || command==='copy_mode') {
      document.querySelector('.pane-focused .terminal-host')?.dispatchEvent(new CustomEvent('luvus:terminal-command',{detail:command}));return;
    }
    // Owner-local topology IDs must never be interpreted in a projected workspace.
    if (workspace.host) {notify(tr('Open the host session to change its layout.'));return;}
    if(command==='new_tab'){await newTab();return;}
    if(command==='rename_tab'){setDialog(topologyDialog('tab-rename',snapshot,workspace,tab));return;}
    if(command==='close_node'){setDialog(topologyDialog('workspace-close',snapshot,workspace));return;}
    if(command==='new_worktree') {
      setDialog({title:tr('New worktree'),fields:[{key:'branch',label:tr('Branch'),required:true}],run:async v=>{
        validateContext(snapshot,workspace,tab,undefined,true);
        const result=record(await mutate('worktree.create',{workspace_id:workspace.id,branch:v.branch},snapshot.generation));
        validateContext(snapshot,workspace,tab,undefined,true);
        return mutate('worktree.open',{path:text(result.path)},snapshot.generation);
      }});return;
    }
    if (command==='resize_mode') {document.querySelector<HTMLElement>('.pane-focused')?.setAttribute('data-resizing','true');return;}
    if (command==='resize:end') {document.querySelector('[data-resizing]')?.removeAttribute('data-resizing');return;}
    if (!focused?.target) {if(command==='close_pane')closeTab(tab);return;}
    if (command==='focus_back' || command==='focus_forward') {await request({type:'focus_history',id:crypto.randomUUID(),target:focused.target,forward:command==='focus_forward'});await refreshSnapshot();return;}
    if (command==='resize:equalize') {
      const equalize=async(tree:Layout|null,path:string[]=[])=>{
        if(!tree || !('Split' in tree)) return;
        validateContext(snapshot,workspace,tab,focused,true);
        await mutate('layout.set_split_ratio',{workspace_id:workspace.id,tab_id:tab.id,path,ratio:0.5},snapshot.generation);
        await equalize(tree.Split.a,[...path,'a']);await equalize(tree.Split.b,[...path,'b']);
      };
      await equalize(tab.layout);return;
    }
    if (command.startsWith('resize:')) {
      const [,direction,cells]=command.split(':');
      try {await mutate('pane.resize',{pane:focused.id,direction,cells:Number(cells)},snapshot.generation);}
      catch(error){if(!String(error).includes('no matching divider can resize this pane'))throw error;}return;
    }
    if (['focus_left','focus_right','focus_up','focus_down'].includes(command)) {
      try {await mutate('pane.focus_direction',{pane:focused.id,direction:command.slice(6)},snapshot.generation);}
      catch(error){if(!String(error).includes('no pane exists in that direction'))throw error;}return;
    }
    if (['split_right','split_down','split_auto'].includes(command)) {await mutate('pane.split',{pane:focused.id,direction:command.slice(6),focus:true},snapshot.generation);return;}
    if (command==='close_pane') {await mutate('pane.close',{pane:focused.id},snapshot.generation);return;}
    if (command==='zoom_pane') {await mutate('pane.zoom',{pane:focused.id},snapshot.generation);return;}
    if (command==='transpose_pane' && tab.panes.length>1) {await mutate('pane.transpose',{pane:focused.id},snapshot.generation);return;}
    if (command==='fork_session' && focused.raw.can_fork===true) await mutate('agent.fork',{target:focused.id,focus:true},snapshot.generation);
  });
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
    commands.push({ id: 'tab-close', label: tr("Close current tab…"), run: () => closeTab(tab) });
    for (const direction of ['left', 'right']) commands.push({ id: `tab-move-${direction}`, label: `Move current tab ${direction}`, run: () => mutate('tab.move', { tab_id: tab.id, direction }, snapshot.generation) });
  }
  if (snapshot && workspace && !workspace.host && tab && focused?.target) {
    for (const forward of [false, true]) commands.push({ id:forward ? 'focus-forward' : 'focus-back', label:forward ? tr("Focus forward") : tr("Focus back"),
      run:() => request({type:'focus_history', id:crypto.randomUUID(), target:focused.target!, forward}) });
    commands.push({ id: 'pane-split', label: tr("Split pane"), run: () => void splitPane(snapshot, workspace, tab, focused).catch(reportError) });
    commands.push({ id: 'agent-start', label: tr("Start an agent…"), run: () => setDialog(topologyDialog('agent-start', snapshot, workspace, tab, focused)) });
  }
  const newBrowser = async (pane: Pane, url = 'about:blank') => {
    if (creatingBrowser.current || !snapshot || !workspace || workspace.host || !pane.target) return;
    validateContext(snapshot, workspace, tab, pane, true);
    if (!array(snapshot.raw.desktop_features).includes('browser_tab')) throw new Error(tr("This running core needs an update to create browser tabs from the desktop."));
    creatingBrowser.current = true;
    try {
      const result = record(await request({type:'browser_tab', id:crypto.randomUUID(), target:pane.target, url}));
      rememberBrowser(snapshot.generation,text(result.pane)); await refreshSnapshot(); setTool(null);
    } finally { creatingBrowser.current = false; }
  };
  const renderTool = (selected: Tool) => {
    if (!snapshot) return <Loading />;
    const props = { snapshot, workspace, openDialog: setDialog };
    const key = `${snapshot.generation}:${workspace?.id ?? ''}:${selected}`;
    switch (selected) {
      case 'mission': return <MissionPanel key={key} {...props} />;
      case 'schedules': return <OrchPanel key={key} schedules {...props} />;
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
  return <div ref={root} className="desktop-shell" data-left-hidden={leftHidden || undefined} data-right-hidden={rightHidden || undefined} style={{ '--terminal-bg': terminalPalette.background, '--terminal-fg': terminalPalette.foreground, '--sidebar-width': `${sidebarWidth}px`, '--tools-width': `${toolsWidth}px` } as CSSProperties}>
    {snapshot ? <Sidebar snapshot={snapshot} openDialog={setDialog} setTool={setTool} onAgent={inspectAgent} onPalette={() => setPalette(true)} /> : <aside className="sidebar"><div className="sidebar-titlebar"><span className="app-wordmark">Luvus</span></div><div className="sidebar-empty">{core.error ? tr("Disconnected") : tr("Connecting to the selected core…")}</div></aside>}
    <div className="left-sidebar-toggle"><IconButton label={tr(leftHidden ? 'Show left sidebar' : 'Hide left sidebar')} aria-expanded={!leftHidden} onClick={() => setLeftHidden(!leftHidden)}>{leftHidden ? <PanelLeftOpen size={15}/> : <PanelLeftClose size={15}/>}</IconButton></div>
    <div className="sidebar-resizer" role="separator" aria-label={tr("Sidebar width")} aria-orientation="vertical" tabIndex={0} aria-valuemin={208} aria-valuemax={400} aria-valuenow={sidebarWidth} onKeyDown={(event) => { if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') { event.preventDefault(); setSidebarWidth((value) => Math.max(208, Math.min(400, value + (event.key === 'ArrowLeft' ? -8 : 8)))); } }} onPointerDown={(event) => event.currentTarget.setPointerCapture(event.pointerId)} onPointerMove={(event) => { if (event.currentTarget.hasPointerCapture(event.pointerId)) setSidebarWidth(Math.max(208, Math.min(400, event.clientX - (root.current?.getBoundingClientRect().left ?? 0)))); }} onPointerUp={(event) => event.currentTarget.releasePointerCapture(event.pointerId)} />
    <main className="main-shell"><header className="topbar"><div className="workspace-context"><FolderOpen size={16} /><strong>{workspace?.name ?? tr("Luvus desktop")}</strong>{workspace?.branch && <span className="branch"><GitBranch size={12} />{workspace.branch}</span>}{workspace?.host && <span className="badge">{workspace.host}</span>}</div><div className="toolbar no-drag">
      <IconButton label={tr("Command palette (Command K)")} onClick={() => setPalette(true)}><Command size={16} /></IconButton>
      <IconButton label={tr("Global finder (Command P)")} onClick={() => setTool(tool === 'search' && !rightHidden ? null : 'search')}><Search size={16} /></IconButton>
      <IconButton label={tr("New browser tab")} disabled={!focused?.target || Boolean(workspace?.host)} onClick={() => { if (focused) void newBrowser(focused).catch(reportError); }}><Globe size={16} /></IconButton>
      <IconButton label={tr("Toggle Commander")} aria-pressed={commander} onClick={() => setCommander(!commander)}><Bot size={16} /></IconButton>
      <IconButton label={tr("Split active pane")} disabled={!focused?.target || Boolean(workspace?.host) || !snapshot || !workspace || !tab} onClick={() => { if (snapshot && workspace && !workspace.host && tab && focused) void splitPane(snapshot, workspace, tab, focused).catch(reportError); }}><Columns2 size={16} /></IconButton>
      <IconButton label={tr(rightHidden ? 'Show right sidebar' : 'Hide right sidebar')} aria-expanded={!rightHidden} onClick={() => setRightHidden(!rightHidden)}>{rightHidden ? <PanelRightOpen size={15}/> : <PanelRightClose size={15}/>}</IconButton>
    </div></header>
    <div className="tabbar" role="tablist" aria-label={tr("Workspace tabs")}>{workspace?.tabs.map((item) => <div className={`tab-item ${item.active && !fullTool ? 'selected' : ''}`} key={item.id || item.index1}><button role="tab" aria-selected={item.active && !fullTool} onClick={() => { setTool(null); if (workspace.host) { const focus = item.panes.find(pane => pane.id === item.focus)?.target; if (focus) void focusTarget(focus).catch(reportError); } else void mutate('tab.focus', { tab_id: item.id }, snapshot?.generation).catch(reportError); }} onDoubleClick={() => { if (snapshot && !workspace.host) setDialog(topologyDialog('tab-rename', snapshot, workspace, item)); }}>{item.panes.length === 1 && item.panes[0]?.kind === 'browser' ? <Globe size={13} /> : item.kind === 'git' ? <GitBranch size={13} /> : item.kind === 'mission_control' ? <LayoutDashboard size={13} /> : item.kind === 'orchestration' ? <Workflow size={13} /> : <Terminal size={13} />}<span>{item.panes.length === 1 && item.panes[0]?.kind === 'browser' ? ((core.browserTitles[item.panes[0].id] || item.name) === 'about:blank' ? tr('New tab') : core.browserTitles[item.panes[0].id] || item.name || tr('New tab')) : item.name || String(item.index1)}</span></button><IconButton disabled={Boolean(workspace.host)} label={`Close ${item.name || `tab ${item.index1}`}`} onClick={() => closeTab(item)}><X size={12} /></IconButton></div>)}<IconButton label={tr("New tab")} disabled={!snapshot || !workspace || Boolean(workspace.host)} onClick={() => { if (snapshot && workspace) void newTab().catch(reportError); }}><Plus size={15} /></IconButton></div>
    {core.error && <div className="global-error"><ErrorBox error={core.error} />{core.ready && <IconButton label={tr("Dismiss error")} onClick={clearError}><X size={14} /></IconButton>}</div>}

    <div className="workbench"><div className="central-stack"><div className="central-content">
      {fullTool ? <div className="full-tool"><div className="full-tool-nav"><button className="text-button" onClick={() => setTool(null)}><ArrowLeft size={14} />{tr("Back to terminals")}</button></div>{renderTool(fullTool)}</div> : !snapshot ? <Empty title={core.error ? tr("Unable to connect to the core") : tr("Connecting to Luvus")} action={core.error && <AsyncButton className="button primary" run={() => window.luvus.reconnect()}>{tr("Reconnect to session")}</AsyncButton>}>{core.error ? tr("The app will not restart your server or fall back to a full TUI.") : tr("Waiting for a compatible desktop projection…")}</Empty> : !workspace ? <Empty title={tr("Your workspace starts here")} action={<button className="button primary" onClick={() => setDialog(openWorkspaceDialog())}><FolderOpen size={16} />{tr("Open workspace")}</button>}>{tr("Open a project directory. The core owns your shells, agents and layout; this window is just a client.")}</Empty> : !workspace.guiAvailable ? <Empty title={tr(projectionLabel(workspace.raw.projection_status))}>{workspace.host}</Empty> : !tab ? <Empty title={tr("No active tab")} /> : tab.kind === 'git' ? renderTool('git') : tab.kind === 'mission_control' ? renderTool('mission') : tab.kind === 'orchestration' ? renderTool('orch') : <WorkspaceArea terminalPalette={terminalPalette} newBrowser={newBrowser} snapshot={snapshot} workspace={workspace} tab={tab} openDialog={setDialog} onAgent={inspectAgent} />}
    </div>{snapshot && <Commander key={snapshot.generation} snapshot={snapshot} visible={commander} openDialog={setDialog} onClose={() => setCommander(false)} />}</div>
    {snapshot && tool && !fullTool && <aside className="tools-panel" aria-label={tr(toolLabels[tool])}><div className="tools-resizer" role="separator" aria-label={tr("Tool panel width")} aria-orientation="vertical" tabIndex={0} aria-valuenow={toolsWidth} aria-valuemin={280} aria-valuemax={680} onKeyDown={(event) => { if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') { event.preventDefault(); setToolsWidth((value) => Math.max(280, Math.min(680, value + (event.key === 'ArrowLeft' ? 8 : -8)))); } }} onPointerDown={(event) => event.currentTarget.setPointerCapture(event.pointerId)} onPointerMove={(event) => { if (event.currentTarget.hasPointerCapture(event.pointerId)) setToolsWidth(Math.max(280, Math.min(680, (root.current?.getBoundingClientRect().right ?? window.innerWidth) - event.clientX - 40))); }} onPointerUp={(event) => event.currentTarget.releasePointerCapture(event.pointerId)} /><div className="tools-close"><IconButton label={tr("Close tools panel")} onClick={() => setTool(null)}><PanelRightClose size={15} /></IconButton></div>{renderTool(tool)}</aside>}
    <nav className="tool-rail" aria-label={tr("Tools")}>{([{ id: 'files', icon: Files }, { id: 'diff', icon: FileDiff }, { id: 'git', icon: GitBranch }, { id: 'agents', icon: Bot }, { id: 'modules', icon: Package }, { id: 'tools', icon: Wrench }] as const).map(({ id, icon: Icon }) => <IconButton key={id} label={tr(toolLabels[id])} aria-pressed={tool === id} onClick={() => setTool(tool === id ? null : id)}><Icon size={17} /></IconButton>)}<div className="rail-spacer" /><IconButton label={tr("Settings")} aria-pressed={tool === 'settings'} onClick={() => setTool(tool === 'settings' ? null : 'settings')}><Settings2 size={17} /></IconButton></nav>
    </div><footer className="statusbar"><span className={`connection-dot ${core.ready ? 'online' : ''}`} /><span>{core.ready ? tr("Connected") : tr("Disconnected")}</span><button onClick={() => setTool('connection')}>{snapshot?.session ?? tr("Core session")}</button>{core.notice ? <button className="status-notice" role="status" title={core.notice} onClick={clearNotice}>{core.notice}</button> : snapshot && <ProjectedBars snapshot={snapshot} />}<span className="status-path" title={focused?.cwd || workspace?.cwd}>{focused?.cwd || workspace?.cwd}</span>{snapshot && <span className="status-generation" title={snapshot.generation}>{tr("generation")} {snapshot.generation.slice(0, 8)}</span>}{snapshot && <NotificationInbox key={snapshot.generation} snapshot={snapshot} />}</footer>
    </main>
    {dialog && <ActionDialog key={dialog.title} spec={dialog} onClose={() => setDialog(null)} />}
    {palette && <CommandPalette commands={commands} onClose={() => setPalette(false)} />}
    {keyHelp && snapshot && <ShortcutHelp snapshot={snapshot} onClose={()=>setKeyHelp(false)}/>}
    {prefix && <div className="prefix-indicator" role="status">{tr('Prefix')} <kbd>{text(record(snapshot?.raw.key_bindings).prefix)}</kbd></div>}
  </div>;
}
