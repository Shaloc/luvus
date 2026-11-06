import { tr } from './i18n';
import { useEffect, useState, type FormEvent } from 'react';
import { FolderOpen, LoaderCircle } from 'lucide-react';
import { currentSnapshot, mutate, notify, useCore, type Method } from './bridge';
import { array, record, message, text, type Pane, type RecordValue, type Snapshot, type Tab, type Workspace } from './model';
import { Choice, ErrorBox, Modal } from './ui';

export interface Field {
  key: string; label: string; kind?: 'text' | 'time' | 'textarea' | 'number' | 'select' | 'checkbox' | 'directory';
  value?: string | boolean; required?: boolean; hint?: string; pattern?: string; options?: { value: string; label: string }[];
}
export interface DialogSpec {
  workspacePicker?: boolean; initialHost?: string; title: string; description?: string; submit?: string; destructive?: boolean; fields?: Field[];
  run: (values: RecordValue) => Promise<unknown>;
  done?: (result: unknown) => void;
}
export type OpenDialog = (dialog: DialogSpec) => void;
export function ActionDialog({ spec, onClose }: { spec: DialogSpec; onClose: () => void }) {
  if (spec.workspacePicker) return <WorkspacePicker spec={spec} onClose={onClose} />;
  return <FieldsDialog spec={spec} onClose={onClose} />;
}
function WorkspacePicker({spec, onClose}: {spec: DialogSpec; onClose: () => void}) {
  const {snapshot, remoteHost} = useCore(), generation = snapshot?.generation;
  const [owners, setOwners] = useState<{host:string;session:string}[]>([]);
  const [selected, setSelected] = useState(spec.initialHost ? '' : 'attached'), [path, setPath] = useState('');
  const [error, setError] = useState(''), [loading, setLoading] = useState(!remoteHost), [busy, setBusy] = useState(false);
  useEffect(() => {
    if (!generation || remoteHost) return;
    let live = true;
    void window.luvus.sessions('list','',generation).then(v => {
      if (!live) return;
      const list = array(record(v).remotes).map(record).map(r => ({host:text(r.host),session:text(r.session)}));
      setOwners(list);
      if (spec.initialHost) { const i = list.findIndex(r => r.host === spec.initialHost && r.session === snapshot?.session);
        const fallback = i >= 0 ? i : list.findIndex(r => r.host === spec.initialHost);
        if (fallback >= 0) setSelected(String(fallback)); }
    }).catch(e => {if (live) setError(message(e));}).finally(() => {if (live) setLoading(false);});
    return () => { live = false; };
  }, [generation, remoteHost, spec.initialHost]);
  const submit = async (e: FormEvent) => {
    e.preventDefault(); if (busy || !generation) return; setBusy(true); setError('');
    try {
      if (selected === 'attached') await mutate('workspace.open',{path,focus:true},generation);
      else {
        const owner = selected && owners[Number(selected)];
        if (!owner) throw Error('Choose a session');
        await window.luvus.sessions('open-workspace',{...owner,path},generation);
        notify(`Opened ${path} · ${owner.host} / ${owner.session}`);
      }
      onClose();
    } catch(e) {setError(message(e));} finally {setBusy(false);}
  };
  return <Modal title={tr("Open workspace")} onClose={() => {if(!busy) onClose();}}><form onSubmit={submit}>
    <div className="field"><span>{tr("Session")} {loading && <LoaderCircle size={11} className="spin inline-spinner" aria-label={tr("Looking for remote sessions…")}/>}</span><Choice label={tr("Workspace session")} value={selected} disabled={busy} options={[{value:'attached',label:`${remoteHost || tr("This computer")} / ${snapshot?.session ?? ''}`},...owners.map((r,i)=>({value:String(i),label:`${r.host} / ${r.session}`}))]} onChange={v => {setSelected(v);setPath('');setError('');}} /></div>
    <label className="field"><span>{tr("Directory")}</span><div className="input-group"><input required aria-label={tr("Directory")} placeholder={selected === 'attached' && !remoteHost ? '' : '/path/to/project'} value={path} onChange={e=>setPath(e.target.value)} />
    {selected === 'attached' && !remoteHost && <button type="button" className="button" onClick={()=>void window.luvus.chooseDirectory().then(v=>{if(v)setPath(v);}).catch(e=>setError(message(e)))}><FolderOpen size={14}/>{tr("Browse")}</button>}</div></label>
    {error && <ErrorBox error={error}/>}<footer className="modal-actions"><button type="button" className="button" disabled={busy} onClick={onClose}>{tr("Cancel")}</button><button className="button primary" disabled={!path || !selected || busy}>{tr("Open workspace")}</button></footer>
  </form></Modal>;
}
function FieldsDialog({ spec, onClose }: { spec: DialogSpec; onClose: () => void }) {
  const [values, setValues] = useState<RecordValue>(Object.fromEntries((spec.fields ?? []).map((field) => [field.key, field.value ?? (field.kind === 'checkbox' ? false : '')])));
  const [busy, setBusy] = useState(false), [error, setError] = useState<string | null>(null);
  const submit = async (event: FormEvent) => {
    event.preventDefault(); setBusy(true); setError(null);
    try {
      for (const field of spec.fields ?? []) if (field.required && field.kind === 'select' && !text(values[field.key])) throw new Error(`Choose ${field.label}.`);
      const result = await spec.run(values); spec.done?.(result); onClose(); }
    catch (error) { setError(message(error)); }
    finally { setBusy(false); }
  };
  return <Modal title={spec.title} onClose={() => { if (!busy) onClose(); }}><form onSubmit={(event) => { void submit(event); }}>
    {spec.description && <p className="form-description">{spec.description}</p>}
    {(spec.fields ?? []).map((field) => <label className="field" key={field.key}><span>{field.label}{field.required && <span className="required"> *</span>}</span>
      {field.kind === 'textarea' ? <textarea rows={5} value={text(values[field.key])} required={field.required} onChange={(event) => setValues({ ...values, [field.key]: event.target.value })} />
        : field.kind === 'select' ? <Choice label={field.label} value={text(values[field.key])} options={[{ value: '', label: tr("Choose…") }, ...field.options ?? []]} onChange={value => setValues({ ...values, [field.key]: value })} />
          : field.kind === 'checkbox' ? <input type="checkbox" checked={values[field.key] === true} onChange={(event) => setValues({ ...values, [field.key]: event.target.checked })} />
            : <div className="input-group"><input type={field.kind === 'number' || field.kind === 'time' ? field.kind : 'text'} value={text(values[field.key])} required={field.required} pattern={field.pattern} onChange={(event) => setValues({ ...values, [field.key]: event.target.value })} />{field.kind === 'directory' && <button type="button" className="button" disabled={busy} onClick={() => { void window.luvus.chooseDirectory().then((path) => { if (path) setValues((old) => ({ ...old, [field.key]: path })); }).catch((error: unknown) => setError(message(error))); }}><FolderOpen size={15} />{tr("Browse")}</button>}</div>}
      {field.hint && <small>{field.hint}</small>}
    </label>)}
    {error && <ErrorBox error={error} />}
    <footer className="modal-actions"><button type="button" className="button" disabled={busy} onClick={onClose}>{tr("Cancel")}</button><button className={`button ${spec.destructive ? 'danger' : 'primary'}`} disabled={busy}>{busy && <LoaderCircle size={14} className="spin" />}{spec.submit ?? tr("Apply")}</button></footer>
  </form></Modal>;
}

/** Fail closed when a menu outlives its workspace, tab, or terminal route. */
export function validateContext(snapshot: Snapshot, workspace?: Workspace, tab?: Tab, pane?: Pane, activeOnly = false) {
  const now = currentSnapshot();
  if (!now || now.generation !== snapshot.generation) throw new Error(tr("The core changed. Reopen this action."));
  const w = workspace && now.workspaces.find((item) => item.id === workspace.id);
  if (workspace && (!w || (activeOnly && !w.active))) throw new Error(tr("The selected workspace changed. Reopen this action."));
  const t = tab && w?.tabs.find((item) => item.id === tab.id);
  if (tab && !t) throw new Error(tr("The selected tab no longer exists."));
  const p = pane && (t ? t.panes : w?.tabs.flatMap((item) => item.panes))?.find((item) => item.id === pane.id);
  if (pane && (!p?.target || !pane.target || p.target.terminal_id !== pane.target.terminal_id)) throw new Error(tr("The selected pane changed or is unavailable."));
}
export const openWorkspaceDialog = (initialHost?: string): DialogSpec => ({ title: tr("Open workspace"), workspacePicker: true, initialHost, run: async () => {} });
export async function splitPane(snapshot: Snapshot, workspace: Workspace, tab: Tab, pane: Pane) {
  validateContext(snapshot, workspace, tab, pane);
  return mutate('pane.split', { pane: pane.id, direction: 'auto', focus: true }, snapshot.generation);
}
export function topologyDialog(kind: 'tab-rename' | 'tab-close' | 'workspace-rename' | 'workspace-close' | 'pane-move' | 'agent-start' | 'agent-prompt', snapshot: Snapshot, workspace: Workspace, tab?: Tab, pane?: Pane): DialogSpec {
  const run = (method: Method, params: RecordValue, activeOnly = false) => { validateContext(snapshot, workspace, tab, pane, activeOnly); return mutate(method, params, snapshot.generation); };
  const name: Field = { key: 'name', label: tr("Name"), required: true };
  switch (kind) {
    case 'workspace-rename': return { title: tr("Rename workspace"), fields: [{ ...name, value: workspace.name }], run: (v) => run('workspace.rename', { workspace_id: workspace.id, name: v.name }) };
    case 'workspace-close': return { title: `Close ${workspace.name}?`, submit: tr("Close workspace"), destructive: true, description: tr("This closes the workspace and its live panes. Unsaved work in terminal programs may be lost."), run: () => run('workspace.close', { workspace_id: workspace.id }) };
    case 'tab-rename': return { title: tr("Rename tab"), fields: [{ ...name, value: tab?.name, required: false, hint: tr("An empty name restores the automatic title.") }], run: (v) => run('tab.rename', { tab_id: tab?.id, name: v.name }, true) };
    case 'tab-close': return { title: tr("Close tab?"), description: tr("All panes in this tab will close. Terminal processes may be stopped."), destructive: true, submit: tr("Close tab"), run: () => run('tab.close', { tab_id: tab?.id }, true) };
    case 'pane-move': return { title: tr("Move pane"), fields: [{ key: 'destination', label: tr("Destination tab"), kind: 'select', value: 'new', options: [{ value: 'new', label: tr("New tab") }, ...workspace.tabs.filter((item) => item.id !== tab?.id && item.kind === 'panes').map((item) => ({ value: item.id, label: item.name || `Tab ${item.index1}` }))] }], run: (v) => {
      if (v.destination === 'new') return run('pane.move', { pane: pane?.id, new_tab: true });
      validateContext(snapshot, workspace, tab, pane);
      const destination = currentSnapshot()?.workspaces.find((item) => item.id === workspace.id)?.tabs.find((item) => item.id === v.destination);
      if (!destination) throw new Error(tr("Destination tab no longer exists."));
      return run('pane.move', { pane: pane?.id, tab: destination.index1 });
    } };

    case 'agent-start': return { title: tr("Start an agent"), submit: tr("Start agent"), description: tr("The core launches and detects the agent in a new sibling terminal. Agent software must already be installed."), fields: [{ ...name, pattern: '[a-z][a-z0-9_-]{0,31}', hint: tr("A unique lowercase name, for example review-worker.") }, { key: 'kind', label: tr("Agent kind"), required: true, pattern: '[a-z][a-z0-9_-]{0,31}', hint: tr("Canonical agent ID from your installed core, e.g. codex or claude.") }, { key: 'direction', label: tr("New pane"), kind: 'select', value: 'auto', options: [{ value: 'auto', label: tr("Automatic split") }, { value: 'right', label: tr("Side by side") }, { value: 'down', label: tr("Stacked") }] }], run: (v) => run('agent.start', { name: v.name, kind: v.kind, anchor: pane?.id, direction: v.direction, timeout_s: 15 }) };
    case 'agent-prompt': return { title: `Prompt ${pane?.title ?? 'agent'}`, submit: tr("Send prompt"), description: tr("Delivery requires fresh core readiness evidence. A queued prompt is not task completion."), fields: [{ key: 'text', label: tr("Prompt"), kind: 'textarea', required: true }], run: (v) => run('agent.prompt', { target: pane?.id, terminal_id: pane?.target?.terminal_id, text: v.text, strict: true }), done: () => notify('Prompt queued by the core; completion is not implied.') };
  }
}
