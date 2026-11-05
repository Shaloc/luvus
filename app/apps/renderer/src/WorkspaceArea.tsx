import { tr } from './i18n';
import { useEffect, useRef, useState } from 'react';
import { ArrowUpRight, Columns2, Copy, MousePointer2, Maximize2, MoreHorizontal, Pencil, Terminal, X } from 'lucide-react';
import { CanvasTerminal, unpackColor } from '@luvus/terminal-view';
import { action, focusTarget, mutate, reportError, request, subscribeTerminal, useCore } from './bridge';
import { topologyDialog, splitPane, validateContext, type OpenDialog } from './Actions';
import { record, targetKey, type Layout, type Pane, type Snapshot, type Tab, type Workspace } from './model';
import { useAppearance, useWallpaper } from './appearance';
import { NativeView } from './NativeView';
import { Empty, IconButton, StatusDot } from './ui';

interface Props { snapshot: Snapshot; workspace: Workspace; tab: Tab; openDialog: OpenDialog; onAgent: (pane: Pane) => void }
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
function PaneView({ pane, snapshot, workspace, tab, openDialog, onAgent }: Props & { pane: Pane }) {
  const local = Boolean(pane.target) && !workspace.host;
  const mouse = record(pane.raw.mouse_options);
  const changeMouse = (field: string) => {
    if (!pane.target) return;
    void request({ type: 'mouse_options', id: crypto.randomUUID(), target: pane.target, right_click_to_app: mouse.right_click_to_app === true, copy_on_select: mouse.copy_on_select === true, [field]: mouse[field] !== true }).catch(reportError);
  };
  const edit = (kind: Parameters<typeof topologyDialog>[0]) => openDialog(topologyDialog(kind, snapshot, workspace, tab, pane));
  return <section className={`pane ${pane.focused ? 'pane-focused' : ''}`} aria-label={`${pane.title} pane`}>
    <div className="pane-header" onPointerDown={(event) => { if (pane.target && event.button === 0 && !(event.target as HTMLElement).closest('button,details')) void focusTarget(pane.target).catch(reportError); }}>
      {pane.isAgent ? <StatusDot state={pane.agentStatus} /> : <Terminal size={13} />}
      <span className="pane-title" title={pane.cwd || pane.title}>{pane.title}</span>
      {pane.isAgent && <button className="text-button" onClick={() => onAgent(pane)}>{pane.agentStatus || tr("Details")}</button>}
      <div className="pane-controls">
        {pane.kind === 'terminal' && <><IconButton label={tr("Right-click passthrough")} disabled={!local || !pane.raw.mouse_options} aria-pressed={mouse.right_click_to_app === true} onClick={() => changeMouse('right_click_to_app')}><MousePointer2 size={13} /></IconButton><IconButton label={tr("Copy on selection")} disabled={!local || !pane.raw.mouse_options} aria-pressed={mouse.copy_on_select === true} onClick={() => changeMouse('copy_on_select')}><Copy size={13} /></IconButton></>}
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
    <div className="pane-body">{pane.kind === 'terminal' ? pane.target?.terminal_id ? <TerminalPane key={targetKey(pane.target)} pane={pane} /> : <Empty title={tr("Terminal route unavailable")}>{tr("Input is disabled until the core supplies an exact terminal lifetime.")}</Empty> : <NativeView pane={pane} />}</div>
  </section>;
}
function TerminalPane({ pane }: { pane: Pane }) {
  const appearance = useAppearance(), wallpaper = useWallpaper();
  const palette = record(useCore().snapshot?.raw.terminal_palette);
  const foreground = unpackColor(Number(palette.foreground), '#f1f3f7'), background = unpackColor(Number(palette.background), '#111214');
  const rightClickToApp = record(pane.raw.mouse_options).right_click_to_app === true;
  const root = useRef<HTMLDivElement>(null), terminal = useRef<CanvasTerminal | null>(null);
  const key = pane.target ? targetKey(pane.target) : '';
  useEffect(() => {
    if (!root.current || !pane.target) return;
    const instance = new CanvasTerminal(root.current, {
      target: pane.target, send: request, onError: reportError,
      onMenu: () => { void window.luvus.terminalMenu(instance.hasSelection(), document.documentElement.lang === 'zh-CN').then(result => {
        if (terminal.current !== instance || !result) return;
        instance.focus();
        if (result.action === 'copy') instance.copy();
        else if (result.action === 'paste') instance.paste(result.text);
      }).catch(reportError); },
      onFocus: () => { if (pane.target) void focusTarget(pane.target, false).catch(reportError); },
      onLink: (url: string) => { void action('browser.open', { pane: pane.id, url }, pane.target?.generation).catch(reportError); },
    });
    terminal.current = instance;
    const unsubscribe = subscribeTerminal(pane.target, (frame) => instance.update(frame));
    return () => { unsubscribe(); instance.dispose(); terminal.current = null; };
  }, [key]);
  useEffect(() => { terminal.current?.setAppearance({ fontSize: appearance.terminalSize, fontFamily: appearance.terminalFont, backgroundOpacity: wallpaper ? 0 : 1, rightClickToApp, foreground, background }); }, [key, appearance.terminalSize, appearance.terminalFont, wallpaper, rightClickToApp, foreground, background]);
  // Clicking a different canvas moves native focus before the owner's snapshot
  // comes back. An older snapshot must never take the keyboard back mid-word.
  useEffect(() => { if (pane.focused && !document.querySelector('dialog[open]') && document.activeElement === document.body) terminal.current?.focus(); }, [pane.focused]);
  return <div className="terminal-host" ref={root} />;
}
