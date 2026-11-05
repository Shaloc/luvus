import { useState, type FormEvent } from 'react';
import { FolderOpen, LoaderCircle } from 'lucide-react';
import { currentSnapshot, mutate, notify, type Method } from './bridge';
import { message, record, text, type Pane, type RecordValue, type Snapshot, type Tab, type Workspace } from './model';
import { ErrorBox, Modal } from './ui';

export interface Field {
  key: string; label: string; kind?: 'text' | 'textarea' | 'number' | 'select' | 'checkbox' | 'directory';
  value?: string | boolean; required?: boolean; hint?: string; pattern?: string; options?: { value: string; label: string }[];
}
export interface DialogSpec {
  title: string; description?: string; submit?: string; destructive?: boolean; fields?: Field[];
  run: (values: RecordValue) => Promise<unknown>;
  done?: (result: unknown) => void;
}
export type OpenDialog = (dialog: DialogSpec) => void;
export function ActionDialog({ spec, onClose }: { spec: DialogSpec; onClose: () => void }) {
  const [values, setValues] = useState<RecordValue>(Object.fromEntries((spec.fields ?? []).map((field) => [field.key, field.value ?? (field.kind === 'checkbox' ? false : '')])));
  const [busy, setBusy] = useState(false), [error, setError] = useState<string | null>(null);
  const submit = async (event: FormEvent) => {
    event.preventDefault(); setBusy(true); setError(null);
    try { const result = await spec.run(values); spec.done?.(result); onClose(); }
    catch (error) { setError(message(error)); }
    finally { setBusy(false); }
  };
  return <Modal title={spec.title} onClose={() => { if (!busy) onClose(); }}><form onSubmit={(event) => { void submit(event); }}>
    {spec.description && <p className="form-description">{spec.description}</p>}
    {(spec.fields ?? []).map((field) => <label className="field" key={field.key}><span>{field.label}{field.required && <span className="required"> *</span>}</span>
      {field.kind === 'textarea' ? <textarea rows={5} value={text(values[field.key])} required={field.required} onChange={(event) => setValues({ ...values, [field.key]: event.target.value })} />
        : field.kind === 'select' ? <select value={text(values[field.key])} required={field.required} onChange={(event) => setValues({ ...values, [field.key]: event.target.value })}>{!field.value && <option value="">Choose…</option>}{field.options?.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}</select>
          : field.kind === 'checkbox' ? <input type="checkbox" checked={values[field.key] === true} onChange={(event) => setValues({ ...values, [field.key]: event.target.checked })} />
            : <div className="input-group"><input type={field.kind === 'number' ? 'number' : 'text'} value={text(values[field.key])} required={field.required} pattern={field.pattern} onChange={(event) => setValues({ ...values, [field.key]: event.target.value })} />{field.kind === 'directory' && <button type="button" className="button" disabled={busy} onClick={() => { void window.luvus.chooseDirectory().then((path) => { if (path) setValues((old) => ({ ...old, [field.key]: path })); }).catch((error: unknown) => setError(message(error))); }}><FolderOpen size={15} />Browse</button>}</div>}
      {field.hint && <small>{field.hint}</small>}
    </label>)}
    {error && <ErrorBox error={error} />}
    <footer className="modal-actions"><button type="button" className="button" disabled={busy} onClick={onClose}>Cancel</button><button className={`button ${spec.destructive ? 'danger' : 'primary'}`} disabled={busy}>{busy && <LoaderCircle size={14} className="spin" />}{spec.submit ?? 'Apply'}</button></footer>
  </form></Modal>;
}

/** Fail closed when a menu outlives its workspace, tab, or terminal route. */
export function validateContext(snapshot: Snapshot, workspace?: Workspace, tab?: Tab, pane?: Pane, activeOnly = false) {
  const now = currentSnapshot();
  if (!now || now.generation !== snapshot.generation) throw new Error('The core changed. Reopen this action.');
  const w = workspace && now.workspaces.find((item) => item.id === workspace.id);
  if (workspace && (!w || (activeOnly && !w.active))) throw new Error('The selected workspace changed. Reopen this action.');
  const t = tab && w?.tabs.find((item) => item.id === tab.id);
  if (tab && !t) throw new Error('The selected tab no longer exists.');
  const p = pane && w?.tabs.flatMap((item) => item.panes).find((item) => item.id === pane.id);
  if (pane && (!p?.target || !pane.target || p.target.terminal_id !== pane.target.terminal_id)) throw new Error('The selected pane changed or is unavailable.');
}
export const openWorkspaceDialog = (): DialogSpec => ({ title: 'Open workspace', submit: 'Open workspace', description: 'Open a local directory in the selected core session. Existing workspaces are focused without creating a second one.', fields: [{ key: 'path', label: 'Directory', kind: 'directory', required: true }], run: (values) => mutate('workspace.open', { path: text(values.path), focus: true }) });
export function topologyDialog(kind: 'tab-new' | 'tab-rename' | 'tab-close' | 'workspace-rename' | 'workspace-close' | 'pane-split' | 'pane-move' | 'pane-close' | 'agent-start' | 'agent-prompt', snapshot: Snapshot, workspace: Workspace, tab?: Tab, pane?: Pane): DialogSpec {
  const run = (method: Method, params: RecordValue, activeOnly = false) => { validateContext(snapshot, workspace, tab, pane, activeOnly); return mutate(method, params, snapshot.generation); };
  const name: Field = { key: 'name', label: 'Name', required: true };
  switch (kind) {
    case 'workspace-rename': return { title: 'Rename workspace', fields: [{ ...name, value: workspace.name }], run: (v) => run('workspace.rename', { workspace_id: workspace.id, name: v.name }) };
    case 'workspace-close': return { title: `Close ${workspace.name}?`, submit: 'Close workspace', destructive: true, description: 'This closes the workspace and its live panes. Unsaved work in terminal programs may be lost.', run: () => run('workspace.close', { workspace_id: workspace.id }) };
    case 'tab-new': return { title: 'New terminal tab', submit: 'Create tab', description: `Start a shell in ${workspace.name}.`, fields: [{ ...name, required: false, hint: 'Optional. Leave empty to use the core title.' }], run: async (v) => {
      const result = await run('tab.new', {}, true);
      const index = text(record(result).tab);
      if (text(v.name) && index) { validateContext(snapshot, workspace, undefined, undefined, true); await mutate('tab.rename', { tab: Number(index), name: v.name }, snapshot.generation); }
      return result;
    } };
    case 'tab-rename': return { title: 'Rename tab', fields: [{ ...name, value: tab?.name, required: false, hint: 'An empty name restores the automatic title.' }], run: (v) => run('tab.rename', { tab_id: tab?.id, name: v.name }, true) };
    case 'tab-close': return { title: 'Close tab?', description: 'All panes in this tab will close. Terminal processes may be stopped.', destructive: true, submit: 'Close tab', run: () => run('tab.close', { tab_id: tab?.id }, true) };
    case 'pane-split': return { title: 'Split pane', submit: 'Create split', fields: [{ key: 'direction', label: 'Arrangement', kind: 'select', value: 'right', options: [{ value: 'right', label: 'Side by side' }, { value: 'down', label: 'Stacked' }, { value: 'auto', label: 'Automatic' }] }], run: (v) => run('pane.split', { pane: pane?.id, direction: v.direction, focus: true }) };
    case 'pane-move': return { title: 'Move pane', fields: [{ key: 'destination', label: 'Destination tab', kind: 'select', value: 'new', options: [{ value: 'new', label: 'New tab' }, ...workspace.tabs.filter((item) => item.id !== tab?.id && item.kind === 'panes').map((item) => ({ value: item.id, label: item.name || `Tab ${item.index1}` }))] }], run: (v) => {
      if (v.destination === 'new') return run('pane.move', { pane: pane?.id, new_tab: true });
      validateContext(snapshot, workspace, tab, pane);
      const destination = currentSnapshot()?.workspaces.find((item) => item.id === workspace.id)?.tabs.find((item) => item.id === v.destination);
      if (!destination) throw new Error('Destination tab no longer exists.');
      return run('pane.move', { pane: pane?.id, tab: destination.index1 });
    } };
    case 'pane-close': return { title: `Close ${pane?.title ?? 'pane'}?`, description: pane?.kind === 'terminal' ? 'The terminal process will be stopped. This is not a detach operation.' : 'Close this native view.', destructive: true, submit: 'Close pane', run: () => run('pane.close', { pane: pane?.id }) };
    case 'agent-start': return { title: 'Start an agent', submit: 'Start agent', description: 'The core launches and detects the agent in a new sibling terminal. Agent software must already be installed.', fields: [{ ...name, pattern: '[a-z][a-z0-9_-]{0,31}', hint: 'A unique lowercase name, for example review-worker.' }, { key: 'kind', label: 'Agent kind', required: true, pattern: '[a-z][a-z0-9_-]{0,31}', hint: 'Canonical agent ID from your installed core, e.g. codex or claude.' }, { key: 'direction', label: 'New pane', kind: 'select', value: 'auto', options: [{ value: 'auto', label: 'Automatic split' }, { value: 'right', label: 'Side by side' }, { value: 'down', label: 'Stacked' }] }], run: (v) => run('agent.start', { name: v.name, kind: v.kind, anchor: pane?.id, direction: v.direction, timeout_s: 15 }) };
    case 'agent-prompt': return { title: `Prompt ${pane?.title ?? 'agent'}`, submit: 'Send prompt', description: 'Delivery requires fresh core readiness evidence. A queued prompt is not task completion.', fields: [{ key: 'text', label: 'Prompt', kind: 'textarea', required: true }], run: (v) => run('agent.prompt', { target: pane?.id, terminal_id: pane?.target?.terminal_id, text: v.text, strict: true }), done: () => notify('Prompt queued by the core; completion is not implied.') };
  }
}
