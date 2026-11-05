import { tr } from './i18n';
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
      if(!note || !Number.isSafeInteger(note.revision)) throw new Error(tr("Reload the review note before changing it."));
      params={...params,if_note_revision:note.revision};
    }
    return mutate(method, params, snapshot.generation).catch(error=>{query.reload();throw error;});
  };
  return <section className="review-notes"><div className="panel-toolbar"><h3>{tr("Review notes")}</h3><button className="button" onClick={() => openDialog({ title: tr("Add review note"), submit: tr("Save note"), fields: [{ key: 'side', label: tr("Side"), kind: 'select', value: 'new', options: [{ value: 'new', label: tr("New version") }, { value: 'old', label: tr("Old version") }] }, { key: 'line', label: tr("Line number"), kind: 'number', required: true }, { key: 'kind', label: tr("Kind"), kind: 'select', value: 'issue', options: ['issue', 'question', 'suggestion', 'praise'].map((value) => ({ value, label: value })) }, { key: 'body', label: tr("Note"), kind: 'textarea', required: true }], run: (v) => {
    const line = Number(v.line); if (!Number.isSafeInteger(line) || line < 1) throw new Error(tr("Line must be a positive integer."));
    return guarded('diff.note.add', { path, layer, [v.side === 'old' ? 'old_line' : 'new_line']: line, kind: v.kind, body: v.body });
  }, done: query.reload })}>{tr("Add note…")}</button></div>
    {query.loading ? <Loading /> : query.error ? <ErrorBox error={query.error} /> : <>{array(record(query.data).notes).map((value) => { const note = record(value); return <article className="task-card" key={text(note.id)}><div className="task-meta"><span className="badge">{text(note.kind)} · {text(note.state)}</span><small className="muted">{text(note.side)}:{String(note.start_line ?? '')}</small></div><p>{text(note.body)}</p><div className="button-row">
      <button className="button" onClick={() => openDialog({ title: tr("Edit review note"), fields: [{ key: 'body', label: tr("Note"), kind: 'textarea', required: true, value: text(note.body) }], run: (v) => guarded('diff.note.edit', { id: note.id, body: v.body }), done: query.reload })}>{tr("Edit…")}</button>
      <button className="button" onClick={() => openDialog({ title: note.state === 'resolved' ? tr("Reopen note?") : tr("Resolve note?"), submit: tr("Confirm"), run: () => guarded(note.state === 'resolved' ? 'diff.note.reopen' : 'diff.note.resolve', { id: note.id }), done: query.reload })}>{note.state === 'resolved' ? tr("Reopen…") : tr("Resolve…")}</button>
      <button className="button" onClick={() => openDialog({ title: tr("Send note to agent"), description: tr("The core sends this review note to the selected agent. Delivery is not task completion."), fields: [{ key: 'to', label: tr("Agent"), kind: 'select', required: true, options: snapshot.workspaces.flatMap((w) => w.tabs.flatMap((t) => t.panes.filter((p) => p.isAgent && p.target).map((p) => ({ value: p.id, label: `${p.title} · ${w.name}` })))) }], run: (v) => guarded('diff.note.send', { ids: [note.id], to: v.to }), done: query.reload })}>{tr("Send…")}</button>
      <button className="text-button destructive-text" onClick={() => openDialog({ title: tr("Delete review note?"), submit: tr("Delete"), destructive: true, run: () => guarded('diff.note.remove', { id: note.id }), done: query.reload })}>{tr("Delete…")}</button>
    </div></article>; })}{!array(record(query.data).notes).length && <Empty title={tr("No review notes")} />}</>}
  </section>;
}
