import { tr } from './i18n';
import { useEffect, useState } from 'react';
import { ChevronDown, ChevronRight, File, FileDiff, Folder, GitBranch, MoreHorizontal, RefreshCw, Search } from 'lucide-react';
import type { Request } from '@luvus/app-protocol';
import { action, mutate, request, useQuery } from './bridge';
import { validateContext, type OpenDialog } from './Actions';
import { array, numeric, record, text, type RecordValue, type Snapshot, type Workspace } from './model';
import { DiffContent } from './NativeView';
import { DiffNotes } from './DiffNotes';
import { AsyncButton, Empty, ErrorBox, IconButton, Loading, Notice } from './ui';

interface Props { snapshot: Snapshot; workspace: Workspace; openDialog: OpenDialog }
export function FilesPanel({ snapshot, workspace, openDialog }: Props) {
  const query = useQuery('files.tree', {}, !workspace.host, true), [filter, setFilter] = useState('');
  const rows = array(record(query.data).rows).map(record).filter((row) => text(row.path).toLocaleLowerCase().includes(filter.toLocaleLowerCase()));
  const open = (path: string) => {
    validateContext(snapshot, workspace, undefined, undefined, true);
    return mutate('files.open', { path, target: 'tab' }, snapshot.generation);
  };
  const entry=(path:string,operation:Extract<Request,{type:'file_entry'}>['operation'])=>{
    validateContext(snapshot,workspace,undefined,undefined,true);
    return request({type:'file_entry',id:crypto.randomUUID(),generation:snapshot.generation,workspace_id:workspace.id,root:text(record(query.data).root),path,operation});
  };
  const confirm=async (row:RecordValue,rename:boolean)=>{
    const path=text(row.path),name=text(row.name);
    const identity=text(record(await entry(path,{kind:'inspect'})).identity);
    if (!identity) throw new Error(tr("The selected file no longer exists."));
    openDialog({title:rename?tr("Rename entry"):`Delete ${name}?`,description:rename?path:`Remove ${path}. Only files, links and empty folders can be deleted here.`,
      submit:rename?tr("Rename"):tr("Delete entry"),destructive:!rename,fields:rename?[{key:'name',label:tr("New name"),required:true,value:name}]:[],
      run:v=>entry(path,rename?{kind:'rename',name:text(v.name),identity}:{kind:'delete',identity}),done:query.reload});
  };
  const create=(path:string)=>openDialog({title:tr("Create entry"),submit:tr("Create"),fields:[
    {key:'kind',label:tr("Kind"),kind:'select',value:'create_file',options:[{value:'create_file',label:tr("File")},{value:'create_directory',label:tr("Folder")}]},
    {key:'name',label:tr("Name"),required:true}],run:v=>entry(path,{kind:v.kind==='create_directory'?'create_directory':'create_file',name:text(v.name)}),done:query.reload});
  if (workspace.host) return <Empty title={tr("File tools require a local owner")} />;
  return <section className="domain-panel files-panel"><div className="panel-toolbar"><h2>{tr("Files")}</h2><IconButton label={tr("Refresh files")} onClick={() => { void action('files.refresh').then(query.reload).catch(() => query.reload()); }}><RefreshCw size={14} /></IconButton></div>
    <div className="filter-input"><Search size={14} /><input aria-label={tr("Filter loaded files")} value={filter} onChange={(event) => setFilter(event.target.value)} placeholder={tr("Filter loaded files…")} /></div>
    <div className="path-caption" title={workspace.cwd}>{workspace.cwd}</div>
    {query.loading ? <Loading /> : query.error ? <ErrorBox error={query.error} /> : <div className="file-tree">{rows.map(row => <div key={text(row.path)} className="file-tree-row" style={{ paddingLeft: `${Math.min(12, numeric(row.depth)) * 12 + 8}px` }}>
      {row.dir === true ? <AsyncButton className="file-button" aria-expanded={row.expanded===true} run={async()=>{await entry(text(row.path),{kind:row.expanded?'collapse':'expand'});query.reload();}}>{row.expanded?<ChevronDown size={12}/>:<ChevronRight size={12}/>}<Folder size={14} /><span title={text(row.path)}>{text(row.name)}</span></AsyncButton> : <AsyncButton className="file-button" run={() => open(text(row.path))}><File size={14} /><span title={text(row.path)}>{text(row.name)}</span></AsyncButton>}
      <details className="dropdown"><summary className="icon-button" aria-label={'File actions for '+text(row.name)}><MoreHorizontal size={13}/></summary><div className="dropdown-menu">
        {row.dir===true && <button onClick={()=>create(text(row.path))}>{tr("New entry here…")}</button>}
        {!row.dir && /\.(md|markdown|mmd|mermaid)$/i.test(text(row.name)) && <AsyncButton run={()=>entry(text(row.path),{kind:/\.(mmd|mermaid)$/i.test(text(row.name))?'mermaid':'markdown'})}>{tr("Open document preview")}</AsyncButton>}
        <AsyncButton run={()=>confirm(row,true)}>{tr("Rename…")}</AsyncButton><AsyncButton run={()=>confirm(row,false)}>{tr("Delete…")}</AsyncButton>
      </div></details>
    </div>)}</div>}
    {!query.loading && !query.error && !rows.length && <Empty title={filter ? tr("No matching loaded files") : tr("No file rows loaded")} />}
    <button className="button" onClick={() => openDialog({ title: tr("Open file"), submit: tr("Open native view"), fields: [{ key: 'path', label: tr("File path"), required: true, hint: tr("Absolute or relative to the active workspace.") }], run: (values) => open(text(values.path)) })}>{tr("Open file by path…")}</button>
    <button className="button" disabled={!text(record(query.data).root)} onClick={()=>create(text(record(query.data).root))}>{tr("New entry…")}</button>
  </section>;
}
export function GitPanel({ snapshot, workspace, openDialog }: Props) {
  const [mode, setMode] = useState<'status' | 'branches' | 'log' | 'pulls'>('status');
  const [prRefresh,setPrRefresh]=useState(0);
  const query = useQuery(mode === 'status' ? 'git.status' : mode === 'branches' ? 'git.branches' : 'git.log', { workspace_id: workspace.id },mode!=='pulls' && !workspace.host);
  const data = record(query.data);
  const openDiff = (path: string, layer: string) => {
    validateContext(snapshot, workspace, undefined, undefined, true);
    return mutate('diff.open', { path, layer, placement: 'tab' }, snapshot.generation);
  };
  if (workspace.host) return <Empty title={tr("Source control requires a local owner")} />;
  return <section className="domain-panel"><div className="panel-toolbar"><h2>{tr("Source control")}</h2><IconButton label={tr("Refresh Git")} onClick={()=>mode==='pulls'?setPrRefresh(n=>n+1):query.reload()}><RefreshCw size={14} /></IconButton></div>
    <div className="segmented" role="tablist" aria-label={tr("Git views")}>{(['status', 'branches', 'log','pulls'] as const).map((item) => <button role="tab" aria-selected={mode === item} key={item} onClick={() => setMode(item)}>{item === 'log' ? tr("History") : item==='pulls'?tr("Pull requests"):item[0]!.toUpperCase() + item.slice(1)}</button>)}</div>
    {mode==='pulls'?<PullRequests key={workspace.id} snapshot={snapshot} workspace={workspace} revision={prRefresh}/>: query.loading ? <Loading /> : query.error ? <ErrorBox error={query.error} /> : mode === 'status' ? <>
      <div className="branch-summary"><GitBranch size={16} /><strong>{text(data.branch, 'Detached HEAD')}</strong><span className="muted">↑ {numeric(data.ahead)} ↓ {numeric(data.behind)}</span></div>
      {(['staged', 'unstaged', 'untracked'] as const).map((group) => <section className="data-section" key={group}><h3>{group}<span>{array(data[group]).length}</span></h3>{array(data[group]).map((item, index) => { const change = record(item), path = typeof item === 'string' ? item : text(change.path); return <AsyncButton key={`${path}:${index}`} className="change-row" run={() => openDiff(path, group === 'staged' ? 'staged' : group === 'untracked' ? 'untracked' : 'worktree')}><span className={`change-code ${text(change.code) === 'A' ? 'added' : ''}`}>{text(change.code, '?')}</span><span>{path}</span><FileDiff size={14} /></AsyncButton>; })}{!array(data[group]).length && <p className="muted small">{tr("No")} {group} {tr("files")}</p>}</section>)}
    </> : mode === 'branches' ? array(data.branches).map((value, index) => { const branch = record(value); return <div className="list-card" key={index}><GitBranch size={15} /><div><strong>{text(branch.name)}</strong><p>{text(branch.subject)}</p></div>{branch.head === true && <span className="badge">{tr("Current")}</span>}</div>; }) : array(data.commits).map((value, index) => { const commit = record(value); return <button className="commit-row" key={index} onClick={() => openDialog({ title: text(commit.subject), description: `${text(commit.sha)} · ${text(commit.author)} · ${text(commit.when)}`, submit: tr("Close"), run: async () => undefined })}><strong>{text(commit.subject)}</strong><small><code>{text(commit.sha).slice(0, 8)}</code> {text(commit.author)} · {text(commit.when)}</small></button>; })}
  </section>;
}
function PullRequests({snapshot,workspace,revision}:{snapshot:Snapshot;workspace:Workspace;revision:number}){
  const [number,setNumber]=useState<number|null>(null),[data,setData]=useState<RecordValue|null>(null),[error,setError]=useState('');
  useEffect(()=>{
    let live=true;setData(null);setError('');
    void request({type:'pull_requests',id:crypto.randomUUID(),generation:snapshot.generation,workspace_id:workspace.id,...(number===null?{}:{number})})
      .then(r=>{if(live)setData(record(r));},e=>{if(live)setError(String(e));});
    return ()=>{live=false;};
  },[workspace.id,snapshot.generation,revision,number]);
  return <>{number!==null && <button className="button" onClick={()=>setNumber(null)}>{tr("Back to pull requests")}</button>}
    {error?<ErrorBox error={error}/>:!data?<Loading/>:number===null?<>{array(data.items).map(record).map(pr=>
      <button className="commit-row" key={numeric(pr.number)} onClick={()=>setNumber(numeric(pr.number))}><strong>#{numeric(pr.number)} {text(pr.title)}</strong>
        <small>{text(pr.author)} · {text(pr.checks)}</small></button>)}{!array(data.items).length && <Empty title={tr("No open pull requests")}/>}</>:
      <article><h3>#{numeric(data.number)} {text(data.title)}</h3><p>{text(data.base)} ← {text(data.head)} · {text(data.state)}</p><p>{text(data.body)}</p>
        <h3>{tr("Checks")}</h3>{array(data.checks).map(record).map((check,i)=><div className="setting-row" key={i}><strong>{text(check.name)}</strong><span>{text(check.status)}</span></div>)}</article>}
  </>;
}
export function DiffPanel({ snapshot, workspace, openDialog }: Props) {
  const [selected, setSelected] = useState<RecordValue | null>(null);
  const query = useQuery('diff.list');
  const selectedPath = text(selected?.path) || text(selected?.new_path) || text(record(selected?.key).new_path) || text(selected?.old_path);
  const layer = text(selected?.layer) || text(record(selected?.key).layer, 'worktree');
  const detail = useQuery('diff.get', { path: selectedPath, layer, include_patch: true }, Boolean(selectedPath));
  const openSplit=()=>{validateContext(snapshot,workspace,undefined,undefined,true);return mutate('diff.open',{path:selectedPath,layer,placement:'tab',view:'split'},snapshot.generation);};
  return <section className="domain-panel"><div className="panel-toolbar"><h2>{tr("Diff review")}</h2><IconButton label={tr("Refresh diff")} onClick={() => { query.reload(); detail.reload(); }}><RefreshCw size={14} /></IconButton></div>
    {query.loading ? <Loading /> : query.error ? <ErrorBox error={query.error} /> : <>
      <div className="change-list">{array(record(query.data).files).map((value, i) => { const file = record(value), path = text(file.path) || text(file.new_path) || text(record(file.key).new_path) || text(file.old_path); return <button key={i} className={`change-row ${selectedPath === path ? 'selected' : ''}`} onClick={() => setSelected(file)}><FileDiff size={14} /><span>{path}</span><span className="added">+{numeric(file.additions)}</span><span className="deleted">−{numeric(file.deletions)}</span></button>; })}</div>
      {!array(record(query.data).files).length && <Empty title={tr("No changes to review")} />}
    </>}
    {selected && <><div className="panel-toolbar"><span className="truncate">{selectedPath}</span><AsyncButton run={() => { validateContext(snapshot, workspace, undefined, undefined, true); return mutate('diff.open', { path: selectedPath, layer, placement: 'tab' }, snapshot.generation); }}>{tr("Open in tab")}</AsyncButton></div>{detail.loading ? <Loading /> : detail.error ? <ErrorBox error={detail.error} /> : <DiffContent data={record(detail.data)} />}
      <AsyncButton run={openSplit}>{tr("Open split diff")}</AsyncButton>
      <DiffNotes key={`${workspace.id}:${selectedPath}:${layer}`} snapshot={snapshot} workspace={workspace} path={selectedPath} layer={layer} openDialog={openDialog} />
    </>}
  </section>;
}
