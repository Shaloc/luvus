import { tr } from './i18n';
import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { ArrowUpRight, Columns2, Copy, ClipboardPaste, Globe, Rows2, Link, MousePointer2, Maximize2, MoreHorizontal, Pencil, Terminal, X } from 'lucide-react';
import { CanvasTerminal } from '@luvus/terminal-view';
import { action, focusTarget, mutate, refreshSnapshot, reportError, request, subscribeTerminal } from './bridge';
import { topologyDialog, splitPane, validateContext, type OpenDialog } from './Actions';
import { array, record, text, targetKey, type Layout, type Pane, type Snapshot, type Tab, type Workspace } from './model';
import { terminalFontStack, useAppearance, useWallpaper } from './appearance';
import { PointerMenu, type MenuPoint, type MenuEntry } from './PointerMenu';
import { NativeView } from './NativeView';
import { Empty, IconButton, StatusDot } from './ui';

interface Props { terminalPalette: {foreground:string; background:string; ansi:readonly string[]}; newBrowser: (pane: Pane,url?:string) => Promise<void>; snapshot: Snapshot; workspace: Workspace; tab: Tab; openDialog: OpenDialog; onAgent: (pane: Pane) => void }
export function WorkspaceArea(props: Props) {
  const { tab } = props;
  const pane = tab.panes.find((item) => item.id === tab.focus);
  if (tab.panes.length === 1 && pane?.kind === 'browser') return <NativeView pane={pane} />;
  if (tab.zoomed && pane) return <PaneView {...props} pane={pane} />;
  if (!tab.layout) return tab.panes.length === 1 && tab.panes[0] ? <PaneView {...props} pane={tab.panes[0]} /> : <Empty title={tr("The core did not supply a split layout")}>{tr("A semantic LayoutTree is required; no layout is guessed from pane order.")}</Empty>;
  return <SplitTree {...props} tree={tab.layout} path={[]} />;
}
function SplitTree(props: Props & { tree: Layout; path: string[] }) {
  const { tree, path } = props;
  if ('Leaf' in tree) {
    const pane = props.tab.panes.find((item) => item.id === String(tree.Leaf));
    return pane ? <PaneView {...props} pane={pane} /> : <Empty title={tr("Pane is no longer available")} />;
  }
  return <Split {...props} split={tree.Split} path={path} />;
}
function Split(props: Props & { split: Extract<Layout, { Split: unknown }>['Split']; path: string[] }) {
  const { split, path, workspace, tab, snapshot } = props;
  const [dragRatio, setDragRatio] = useState<number | null>(null);
  const root = useRef<HTMLDivElement>(null), last = useRef(split.ratio);
  const horizontal = split.axis === 0, ratio = dragRatio ?? split.ratio;
  const commit = (value: number) => {
    void mutate('layout.set_split_ratio', { workspace_id: workspace.id, tab_id: tab.id, path, ratio: value }, snapshot.generation).catch(reportError).finally(() => setDragRatio(null));
  };
  return <div ref={root} className={`split-tree ${horizontal ? 'split-columns' : 'split-rows'}`}>
    <div className="split-child" style={{ flex: `${ratio} 1 0` }}><SplitTree {...props} tree={split.a} path={[...path, 'a']} /></div>
    {workspace.host ? <div className="split-divider" /> : <div className="split-divider" role="separator" aria-orientation={horizontal ? 'vertical' : 'horizontal'} aria-label={tr("Resize split")} aria-valuemin={10} aria-valuemax={90} aria-valuenow={Math.round(ratio * 100)} tabIndex={0}
      onPointerDown={(event) => { event.preventDefault(); event.currentTarget.setPointerCapture(event.pointerId); last.current = ratio; setDragRatio(ratio); }}
      onPointerMove={(event) => {
        if (!event.currentTarget.hasPointerCapture(event.pointerId)) return;
        const rect = root.current?.getBoundingClientRect(); if (!rect) return;
        last.current = Math.max(0.1, Math.min(0.9, horizontal ? (event.clientX - rect.left) / rect.width : (event.clientY - rect.top) / rect.height));
        setDragRatio(last.current);
      }}
      onPointerUp={(event) => { if (!event.currentTarget.hasPointerCapture(event.pointerId)) return; event.currentTarget.releasePointerCapture(event.pointerId); commit(last.current); }}
      onPointerCancel={() => setDragRatio(null)}
      onKeyDown={(event) => {
        const keys = horizontal ? ['ArrowLeft', 'ArrowRight'] : ['ArrowUp', 'ArrowDown'];
        if (!keys.includes(event.key)) return;
        event.preventDefault(); commit(Math.max(0.1, Math.min(0.9, ratio + (event.key === keys[0] ? -0.05 : 0.05))));
      }} />}
    <div className="split-child" style={{ flex: `${1 - ratio} 1 0` }}><SplitTree {...props} tree={split.b} path={[...path, 'b']} /></div>
  </div>;
}
function PaneView({ pane, snapshot, workspace, tab, openDialog, onAgent, newBrowser, terminalPalette }: Props & { pane: Pane }) {
  const local = Boolean(pane.target) && !workspace.host;
  const mouse = record(pane.raw.mouse_options);
  const [changingMouse, setChangingMouse] = useState(false);
  const changeMouse = (field: string) => {
    if (!pane.target || changingMouse) return;
    setChangingMouse(true);
    void request({ type: 'mouse_options', id: crypto.randomUUID(), target: pane.target, right_click_to_app: mouse.right_click_to_app === true, copy_on_select: mouse.copy_on_select === true, [field]: mouse[field] !== true })
      .then(refreshSnapshot).catch(reportError).finally(() => setChangingMouse(false));
  };
  const edit = (kind: Parameters<typeof topologyDialog>[0]) => openDialog(topologyDialog(kind, snapshot, workspace, tab, pane));
  const showPath = pane.kind === 'terminal' && record(snapshot.config.layout).pane_title_path === true && pane.cwd && pane.cwd !== pane.title;
  return <section className={`pane ${pane.focused ? 'pane-focused' : ''}`} aria-label={`${pane.title} pane`}>
    <div className="pane-header" onPointerDown={(event) => { if (pane.target && event.button === 0 && !(event.target as HTMLElement).closest('button,details')) void focusTarget(pane.target).catch(reportError); }}>
      {pane.isAgent ? <StatusDot state={pane.agentStatus} /> : <Terminal size={13} />}
      <span className="pane-title" title={[pane.title, pane.cwd].filter(Boolean).join('\n')}>{pane.title}{showPath && <small> · {pane.cwd}</small>}</span>
      {pane.isAgent && <button className="text-button" onClick={() => onAgent(pane)}>{pane.agentStatus || tr("Details")}</button>}
      <div className="pane-controls">
        {pane.kind === 'terminal' && <><IconButton label={tr("Right-click passthrough")} disabled={!local || !pane.raw.mouse_options || changingMouse} aria-pressed={mouse.right_click_to_app === true} onClick={() => changeMouse('right_click_to_app')}><MousePointer2 size={13} /></IconButton><IconButton label={tr("Copy on selection")} disabled={!local || !pane.raw.mouse_options || changingMouse} aria-pressed={mouse.copy_on_select === true} onClick={() => changeMouse('copy_on_select')}><Copy size={13} /></IconButton></>}
        <IconButton label={tr("Split pane")} disabled={!local} onClick={() => { void splitPane(snapshot, workspace, tab, pane).catch(reportError); }}><Columns2 size={13} /></IconButton>
        <IconButton label={tab.zoomed ? tr("Unzoom pane") : tr("Zoom pane")} disabled={!local} onClick={() => { void mutate('pane.zoom', { pane: pane.id }, snapshot.generation).catch(reportError); }}><Maximize2 size={13} /></IconButton>
        <details className="dropdown"><summary className="icon-button" aria-label={tr("Pane actions")}><MoreHorizontal size={15} /></summary><div className="dropdown-menu">
          <button disabled={!local} onClick={() => edit('pane-move')}><ArrowUpRight size={14} />{tr("Move to tab…")}</button>
          {pane.kind === 'terminal' && <button disabled={!local} onClick={() => edit('agent-start')}>{tr("Start agent…")}</button>}
          {pane.isAgent && <button disabled={!local} onClick={() => edit('agent-prompt')}>{tr("Send prompt…")}</button>}
          <button disabled={!local || tab.panes.length < 2} onClick={() => { void mutate('pane.transpose', { pane: pane.id }, snapshot.generation).catch(reportError); }}>{tr("Transpose split")}</button>
          <button disabled={!local || tab.panes.length < 2} onClick={() => openDialog({ title: tr("Swap pane positions"), fields: [{ key: 'with', label: tr("Swap with"), kind: 'select', required: true, options: tab.panes.filter((other) => other.id !== pane.id && other.target).map((other) => ({ value: other.id, label: other.title })) }], run: (v) => { validateContext(snapshot, workspace, tab, pane); return mutate('pane.swap', { pane: pane.id, with: v.with }, snapshot.generation); } })}>{tr("Swap with pane…")}</button>
          {pane.kind === 'terminal' && <button disabled={!local} onClick={() => openDialog({ title: tr("Restart terminal?"), description: tr("The core stops this terminal and creates a new terminal lifetime. Current processes may be lost."), submit: tr("Restart terminal"), destructive: true, run: () => { validateContext(snapshot, workspace, tab, pane); return mutate('pane.restart', { pane: pane.id }, snapshot.generation); } })}>{tr("Restart terminal…")}</button>}
          <button disabled={!local} onClick={() => openDialog({ title: tr("Rename pane"), fields: [{ key: 'title', label: tr("Name"), pattern: '[a-z][a-z0-9_-]{0,31}', hint: tr("Lowercase alias; an empty name restores the automatic title.") }], run: (v) => mutate('pane.rename', { pane: pane.id, name: v.title }, snapshot.generation) })}><Pencil size={14} />{tr("Rename…")}</button>
          <button className="destructive-text" disabled={!local} onClick={() => { void (async () => { validateContext(snapshot, workspace, tab, pane); await mutate('pane.close', { pane: pane.id }, snapshot.generation); })().catch(reportError); }}><X size={14} />{tr("Close pane")}</button>
        </div></details>
      </div>
    </div>
    <div className="pane-body">{pane.kind === 'terminal' ? pane.target?.terminal_id ? <TerminalPane key={targetKey(pane.target)} {...{pane,snapshot,workspace,tab,openDialog,newBrowser,terminalPalette}} /> : <Empty title={tr("Terminal route unavailable")}>{tr("Input is disabled until the core supplies an exact terminal lifetime.")}</Empty> : <NativeView pane={pane} />}</div>
  </section>;
}
function TerminalPane({ pane, snapshot, workspace, tab, openDialog, newBrowser, terminalPalette }: Omit<Props,'onAgent'> & {pane:Pane}) {
  const [menu,setMenu] = useState<(MenuPoint & {link?:string}) | null>(null);
  const appearance = useAppearance(), wallpaper = useWallpaper();
  const { foreground, background, ansi } = terminalPalette;
  const rightClickToApp = record(pane.raw.mouse_options).right_click_to_app === true;
  const root = useRef<HTMLDivElement>(null), terminal = useRef<CanvasTerminal | null>(null);
  const key = pane.target ? targetKey(pane.target) : '';
  useLayoutEffect(() => {
    if (!root.current || !pane.target) return;
    let disposed = false;
    const instance = new CanvasTerminal(root.current, {
      target: pane.target, send: request, onError: reportError,
      onMenu: (x,y,link) => setMenu({x,y,link}),
      onFocus: () => { if (pane.target) void focusTarget(pane.target, false).catch(error => { if (!disposed) reportError(error); }); },
      onLink: (url: string) => { void action('browser.open', { pane: pane.id, url }, pane.target?.generation).catch(reportError); },
    });
    terminal.current = instance;
    const command = (event: Event) => {instance.command((event as CustomEvent<string>).detail);};
    const host=root.current;
    host.addEventListener('luvus:terminal-command',command);
    const unsubscribe = subscribeTerminal(pane.target, (frame) => instance.update(frame), frame => instance.preview(frame));
    return () => { disposed = true;host.removeEventListener('luvus:terminal-command',command);unsubscribe(); instance.dispose(); terminal.current = null; };
  }, [key]);
  useLayoutEffect(() => { terminal.current?.setAppearance({ fontSize: appearance.terminalSize, fontFamily: terminalFontStack(appearance.terminalFont), fontWeight: appearance.terminalWeight, fontBoldWeight: appearance.terminalBoldWeight, backgroundOpacity: wallpaper ? 0 : 1, rightClickToApp, foreground, background, ansi }); }, [key, appearance.terminalSize, appearance.terminalFont, appearance.terminalWeight, appearance.terminalBoldWeight, wallpaper, rightClickToApp, foreground, background, ansi]);
  // Clicking a different canvas moves native focus before the owner's snapshot
  // comes back. An older snapshot must never take the keyboard back mid-word.
  useEffect(() => { if (pane.focused && !document.querySelector('dialog[open]') && document.activeElement === document.body) terminal.current?.focus(); }, [pane.focused]);
  const local = !workspace.host;
  const checked = (fn:()=>Promise<unknown>) => () => {
    void (async()=>{validateContext(snapshot,workspace,tab,pane,true); await fn();})().catch(reportError);
  };
  const split = (direction:string) => checked(()=>mutate('pane.split',{pane:pane.id,direction,focus:true},snapshot.generation));
  const entries: MenuEntry[] = menu ? [
    {label:tr("Copy"), icon:<Copy size={13}/>, disabled:!terminal.current?.hasSelection(), run:()=>terminal.current?.copy()},
    {label:tr("Paste"), icon:<ClipboardPaste size={13}/>, run:checked(async()=>{terminal.current?.focus();await window.luvus.terminalPaste(pane.target!);})},
    ...(menu.link && local ? [{label:tr("Open link"), icon:<Link size={13}/>, run:checked(()=>newBrowser(pane,menu.link))}] : []),
    {label:tr("Split pane"), icon:<Columns2 size={13}/>, group:true, disabled:!local, run:split('auto')},
    {label:tr("Split right"), icon:<Columns2 size={13}/>, disabled:!local, run:split('right')},
    {label:tr("Split down"), icon:<Rows2 size={13}/>, disabled:!local, run:split('down')},
    {label:tr("New browser tab"), icon:<Globe size={13}/>, disabled:!local, run:checked(()=>newBrowser(pane))},
    {label:tr("Running command"), icon:<Terminal size={13}/>, disabled:!local, run:checked(async()=>{
      const result=record(await action('pane.processes',{pane:pane.id},snapshot.generation));
      validateContext(snapshot,workspace,tab,pane,true);
      if(result.terminal_id !== pane.target?.terminal_id) return;
      openDialog({title:tr("Running command"),description:array(result.executables).map(v=>text(v)).filter(Boolean).join(' · ') || tr("Not available"),submit:tr("Close"),run:async()=>{}});
    })},
    {label:tr("Move to tab…"), icon:<ArrowUpRight size={13}/>, disabled:!local || (tab.panes.length===1 && workspace.tabs.length===1), run:()=>openDialog(topologyDialog('pane-move',snapshot,workspace,tab,pane))},
    ...(tab.panes.length > 1 ? [{label:tr("Transpose split"), icon:<Columns2 size={13}/>, disabled:!local, run:checked(()=>mutate('pane.transpose',{pane:pane.id},snapshot.generation))}] : []),
    {label:tr(tab.zoomed ? "Unzoom pane" : "Zoom pane"), icon:<Maximize2 size={13}/>, disabled:!local, run:checked(()=>mutate('pane.zoom',{pane:pane.id},snapshot.generation))},
    ...(pane.raw.can_fork === true && local ? [{label:tr("Fork session"),run:checked(()=>mutate('agent.fork',{target:pane.id,focus:true},snapshot.generation))}] : []),
    {label:tr("Rename…"), icon:<Pencil size={13}/>, group:true, disabled:!local, run:()=>openDialog({title:tr("Rename pane"),fields:[{key:'name',label:tr("Name")}],run:v=>{validateContext(snapshot,workspace,tab,pane,true);return mutate('pane.rename',{pane:pane.id,name:v.name},snapshot.generation);}})},
    {label:tr("Close pane"), icon:<X size={13}/>, danger:true, disabled:!local, run:checked(()=>mutate('pane.close',{pane:pane.id},snapshot.generation))},
  ] : [];
  return <><div className="terminal-host" ref={root} />{menu && <PointerMenu point={menu} entries={entries} close={()=>setMenu(null)}/>}</>;
}
