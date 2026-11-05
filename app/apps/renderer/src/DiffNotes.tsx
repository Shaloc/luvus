import { mutate, useQuery } from './bridge';
import { validateContext, type OpenDialog } from './Actions';
import { array, record, text, type RecordValue, type Snapshot, type Workspace } from './model';
import { Empty, ErrorBox, Loading } from './ui';

export function DiffNotes({ snapshot, workspace, path, layer, openDialog }: { snapshot: Snapshot; workspace: Workspace; path: string; layer: string; openDialog: OpenDialog }) {
  const query = useQuery('diff.note.list', { path });
  const guarded = (method: 'diff.note.add' | 'diff.note.edit' | 'diff.note.remove' | 'diff.note.resolve' | 'diff.note.reopen' | 'diff.note.send', params: RecordValue) => {
    validateContext(snapshot, workspace, undefined, undefined, true);
    if (typeof params.id==='string') {
      const note=array(record(query.data).notes).map(record).find(n=>n.id===params.id);
      if(!note || !Number.isSafeInteger(note.revision)) throw new Error('Reload the review note before changing it.');
      params={...params,if_note_revision:note.revision};
    }
    return mutate(method, params, snapshot.generation).catch(error=>{query.reload();throw error;});
  };
  return <section className="review-notes"><div className="panel-toolbar"><h3>Review notes</h3><button className="button" onClick={() => openDialog({ title: 'Add review note', submit: 'Save note', fields: [{ key: 'side', label: 'Side', kind: 'select', value: 'new', options: [{ value: 'new', label: 'New version' }, { value: 'old', label: 'Old version' }] }, { key: 'line', label: 'Line number', kind: 'number', required: true }, { key: 'kind', label: 'Kind', kind: 'select', value: 'issue', options: ['issue', 'question', 'suggestion', 'praise'].map((value) => ({ value, label: value })) }, { key: 'body', label: 'Note', kind: 'textarea', required: true }], run: (v) => {
    const line = Number(v.line); if (!Number.isSafeInteger(line) || line < 1) throw new Error('Line must be a positive integer.');
    return guarded('diff.note.add', { path, layer, [v.side === 'old' ? 'old_line' : 'new_line']: line, kind: v.kind, body: v.body });
  }, done: query.reload })}>Add note…</button></div>
    {query.loading ? <Loading /> : query.error ? <ErrorBox error={query.error} /> : <>{array(record(query.data).notes).map((value) => { const note = record(value); return <article className="task-card" key={text(note.id)}><div className="task-meta"><span className="badge">{text(note.kind)} · {text(note.state)}</span><small className="muted">{text(note.side)}:{String(note.start_line ?? '')}</small></div><p>{text(note.body)}</p><div className="button-row">
      <button className="button" onClick={() => openDialog({ title: 'Edit review note', fields: [{ key: 'body', label: 'Note', kind: 'textarea', required: true, value: text(note.body) }], run: (v) => guarded('diff.note.edit', { id: note.id, body: v.body }), done: query.reload })}>Edit…</button>
      <button className="button" onClick={() => openDialog({ title: note.state === 'resolved' ? 'Reopen note?' : 'Resolve note?', submit: 'Confirm', run: () => guarded(note.state === 'resolved' ? 'diff.note.reopen' : 'diff.note.resolve', { id: note.id }), done: query.reload })}>{note.state === 'resolved' ? 'Reopen…' : 'Resolve…'}</button>
      <button className="button" onClick={() => openDialog({ title: 'Send note to agent', description: 'The core sends this review note to the selected agent. Delivery is not task completion.', fields: [{ key: 'to', label: 'Agent', kind: 'select', required: true, options: snapshot.workspaces.flatMap((w) => w.tabs.flatMap((t) => t.panes.filter((p) => p.isAgent && p.target).map((p) => ({ value: p.id, label: `${p.title} · ${w.name}` })))) }], run: (v) => guarded('diff.note.send', { ids: [note.id], to: v.to }), done: query.reload })}>Send…</button>
      <button className="text-button destructive-text" onClick={() => openDialog({ title: 'Delete review note?', submit: 'Delete', destructive: true, run: () => guarded('diff.note.remove', { id: note.id }), done: query.reload })}>Delete…</button>
    </div></article>; })}{!array(record(query.data).notes).length && <Empty title="No review notes" />}</>}
  </section>;
}
