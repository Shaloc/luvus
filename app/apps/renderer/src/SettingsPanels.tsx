import { tr } from './i18n';
import { useEffect, useRef, useState } from 'react';
import { Package, RefreshCw, Search } from 'lucide-react';
import { mutate, request, useCore, useQuery } from './bridge';
import { type OpenDialog } from './Actions';
import { array, label, record, text, truth, type RecordValue, type Snapshot } from './model';
import { AsyncButton, Choice, Detail, Empty, ErrorBox, IconButton, Loading, Notice } from './ui';
import { ProjectedDocks } from './ProjectedWidgets';

interface Props { snapshot: Snapshot; openDialog: OpenDialog }
export { SettingsPanel } from './SettingsPage';
export function LocalAddons({generation}:{generation:string}){
  const {remoteHost}=useCore();const [inventory,setInventory]=useState<RecordValue|null>(null),[error,setError]=useState('');
  useEffect(()=>{let live=true;if(!remoteHost)window.luvus.addons('list','',generation).then(v=>{if(live)setInventory(record(v));}).catch(e=>{if(live)setError(String(e));});return()=>{live=false;};},[remoteHost,generation]);
  if(remoteHost)return <Notice>{tr("Agent installations are managed on their owner.")}</Notice>;
  if(error)return <ErrorBox error={error}/>;
  if(!inventory)return <Loading/>;
  const change=async(op:'install'|'uninstall'|'enable'|'disable',agent='')=>setInventory(record(await window.luvus.addons(op,agent,generation)));
  return <><h3>{tr("Local agent integrations")}</h3>{array(inventory.integrations).map(record).map(row=><div className="list-card" data-agent={text(row.agent)} key={text(row.agent)}><strong>{text(row.agent)}</strong><AsyncButton run={()=>change(row.installed?'uninstall':'install',text(row.agent))}>{row.installed?tr("Uninstall…"):tr("Install…")}</AsyncButton></div>)}
    <h3>{tr("Bundled skill")}</h3>{array(inventory.skills).map(record).map(row=><div className="setting-row" key={text(row.host)}><strong>{text(row.host)}</strong><small>{text(row.state)}</small></div>)}
    <div className="button-row"><AsyncButton run={()=>change('enable')}>{tr("Enable skill…")}</AsyncButton><AsyncButton run={()=>change('disable')}>{tr("Disable skill…")}</AsyncButton></div></>;
}
export function flatten(value: RecordValue, path: string[] = []): [string[], unknown][] {
  return Object.entries(value).flatMap(([key, item]) => item !== null && typeof item === 'object' && !Array.isArray(item) && Object.keys(record(item)).length ? flatten(record(item), [...path, key]) : [[[...path, key], item] as [string[], unknown]]);
}
export function nest(path: string[], value: unknown): RecordValue {
  return path.reduceRight<unknown>((child, key) => ({ [key]: child }), value) as RecordValue;
}
export function SettingRow({ path, value, save, secret = false, options }: { path: string[]; value: unknown; save: (next: unknown) => Promise<unknown>; secret?: boolean; options?: string[] }) {
  const isJson = typeof value === 'object' && !secret;
  const incoming=secret ? '' : typeof value === 'boolean' ? value : isJson ? JSON.stringify(value) : String(value ?? '');
  const [draft,setDraft]=useState(incoming),previous=useRef(incoming);
  useEffect(()=>{
    const was=previous.current;previous.current=incoming;
    setDraft(current=>current===was?incoming:current);
  },[incoming]);
  const title = path.at(-1) ?? '', group = path.slice(0, -1).join(' › ');
  return <div className="setting-row"><div><strong>{secret ? label(title) : tr(label(title))}</strong>{group && <small>{tr(group)}</small>}</div><div className="setting-control">
    {typeof draft === 'boolean' ? <input type="checkbox" aria-label={path.join('.')} checked={draft} onChange={(event) => setDraft(event.target.checked)} /> : options?.length ? <Choice label={path.join('.')} value={String(draft)} onChange={setDraft} options={options.map(value => ({ value, label: value }))} /> : <input aria-label={path.join('.')} type={secret ? 'password' : typeof value === 'number' ? 'number' : 'text'} value={draft} placeholder={secret ? tr("Enter new secret") : undefined} onChange={(event) => setDraft(event.target.value)} />}
    <AsyncButton run={async () => { let next: unknown = draft; if (isJson) { try { next = JSON.parse(String(draft)); } catch { throw new Error(tr("Enter a valid JSON value for this structured setting.")); } } else if (typeof value === 'number') { next = Number(draft); if (!Number.isFinite(next)) throw new Error(tr("Enter a finite number.")); } return save(next); }}>{tr("Save")}</AsyncButton>
  </div></div>;
}
export function ModulesPanel({ snapshot, openDialog }: Props) {
  const query = useQuery('module.list');
  const [selected, setSelected] = useState<string | null>(null), [mode, setMode] = useState<'info' | 'settings' | 'logs'>('info');
  const detail = useQuery(mode === 'settings' ? 'module.settings.list' : mode === 'logs' ? 'module.log.list' : 'module.info', { id: selected }, Boolean(selected));
  const data = record(detail.data);
  return <section className="domain-panel"><div className="panel-toolbar"><h2>{tr("Modules")}</h2><IconButton label={tr("Refresh modules")} onClick={() => { query.reload(); detail.reload(); }}><RefreshCw size={14} /></IconButton></div>
    {query.loading ? <Loading /> : query.error ? <ErrorBox error={query.error} /> : <>{array(record(query.data).modules).map((value) => { const module = record(value); return <div className={`list-card ${selected === module.id ? 'selected' : ''}`} key={text(module.id)}><Package size={16} /><button className="module-select" onClick={() => setSelected(text(module.id))}><strong>{text(module.name)}</strong><small>{text(module.id)} · {text(module.version)}</small></button><AsyncButton run={async () => { await mutate(truth(module.enabled) ? 'module.disable' : 'module.enable', { id: module.id }, snapshot.generation); query.reload(); detail.reload(); }}>{truth(module.enabled) ? tr("Disable") : tr("Enable")}</AsyncButton></div>; })}{!array(record(query.data).modules).length && <Empty title={tr("No modules installed")}>{tr("Link a reviewed module directory to extend the core.")}</Empty>}</>}
    <button className="button" onClick={() => openDialog({ title: tr("Link module directory"), submit: tr("Link module"), description: tr("Modules execute out of process with the core’s authority. Only link code you trust."), fields: [{ key: 'path', label: tr("Module directory"), kind: 'directory', required: true }], run: (v) => mutate('module.link', { path: v.path }, snapshot.generation), done: query.reload })}>{tr("Link module…")}</button>
    <ProjectedDocks snapshot={snapshot} openDialog={openDialog} />
    {selected && <><div className="segmented" role="tablist" aria-label={tr("Module details")}>{(['info', 'settings', 'logs'] as const).map((item) => <button key={item} role="tab" aria-selected={mode === item} onClick={() => setMode(item)}>{item}</button>)}</div>{detail.loading ? <Loading /> : detail.error ? <ErrorBox error={detail.error} /> : mode === 'settings' ? array(data.settings).map((value) => { const setting = record(value); return <SettingRow key={`${text(setting.key)}`} path={[selected, text(setting.key)]} value={setting.value} secret={truth(setting.secret)} options={array(setting.options).filter((value): value is string => typeof value === 'string')} save={(next) => mutate('module.settings.set', { id: selected, key: setting.key, value: next }, snapshot.generation)} />; }) : mode === 'logs' ? <Detail value={data.logs ?? data} /> : <><p className="muted">{text(data.description)}</p><Detail value={{ source: data.source, root: data.root, warning: data.warning }} /><h3>{tr("Actions")}</h3>{array(data.actions).map((value, i) => { const item = record(value); return <button className="button" key={i} onClick={() => openDialog({ title: text(item.title), description: `Run ${selected}.${text(item.id)} through the core module runner.`, submit: tr("Run action"), run: () => mutate('module.action.invoke', { module: selected, id: item.id }, snapshot.generation) })}>{text(item.title) || text(item.id)}</button>; })}</>}</>}
  </section>;
}
export function ConnectionPanel({ snapshot, openDialog }: Props) {
  const query = useQuery('ping');
  const {remoteHost} = useCore();
  const [inventory,setInventory] = useState<RecordValue|null>(null), [inventoryError,setInventoryError] = useState('');
  useEffect(() => {
    if (remoteHost) return;
    let live = true;
    window.luvus.sessions('list','',snapshot.generation).then(result => {
      if (live) setInventory(record(result));
    }).catch(error => {if (live) setInventoryError(String(error));});
    return () => {live = false;};
  }, [snapshot.generation, remoteHost]);
  const operate = (op: 'switch'|'create'|'stop'|'delete'|'list'|'host-add'|'merge', name='') => window.luvus.sessions(op,name,snapshot.generation);
  const refresh = async () => {setInventory(record(await operate('list')));setInventoryError('');};
  return <section className="domain-panel connection-panel"><h2>{tr("Core connection")}</h2>{inventoryError && <ErrorBox error={inventoryError}/>}
    {remoteHost && <p>{remoteHost} / {snapshot.session}</p>}
    {!remoteHost && <><div className="panel-toolbar"><h3>{tr("Local sessions")}</h3><AsyncButton run={refresh}>{tr("List sessions")}</AsyncButton>
      <button className="button" onClick={()=>openDialog({title:tr("Create local session"),submit:tr("Create and switch"),fields:[{key:'name',label:tr("Session name"),required:true,pattern:'[a-zA-Z0-9_.][a-zA-Z0-9_.\\-]{0,63}'}],run:v=>operate('create',text(v.name))})}>{tr("New session…")}</button></div>
      {array(inventory?.sessions).map(record).map(row=><div className="list-card" key={text(row.name)}><div><strong>{text(row.name)}</strong><small>{row.running?tr("Running"):tr("Stopped")}</small></div>
        {row.name===snapshot.session ? <span className="badge">{tr("Attached")}</span> : <><AsyncButton run={()=>operate('switch',text(row.name))}>{tr("Switch")}</AsyncButton>{(row.running || row.name!=='default') && <AsyncButton run={async()=>{await operate(row.running?'stop':'delete',text(row.name));await refresh();}}>{row.running?tr("Stop…"):tr("Delete…")}</AsyncButton>}</>}
      </div>)}
      {inventory && <><div className="panel-toolbar"><h3>{tr("Registered SSH hosts")}</h3><button className="button" onClick={()=>openDialog({title:tr("Connect registered host"),description:tr("Use an existing SSH Host alias. The host key and installed Luvus version must already be compatible."),submit:tr("Connect host"),fields:[{key:'alias',label:tr("SSH alias"),required:true}],run:async v=>setInventory(record(await operate('host-add',text(v.alias))))})}>{tr("Add host…")}</button></div>
        {array(inventory.hosts).map(record).map(h=><div className="list-card" key={text(h.host)}><div><strong>{text(h.host)}</strong><small title={text(h.error)}>{h.error ? tr("Disconnected") : tr("Connected")}</small></div></div>)}
        <label className="setting-row">{tr("Merge same-name sessions")}<input type="checkbox" checked={inventory.merge===true} onChange={e=>{const enabled=e.target.checked;openDialog({title:enabled?tr("Merge same-name sessions?"):tr("Disconnect merged projections?"),submit:enabled?tr("Enable merge"):tr("Disable merge"),description:tr("Applies to local sessions. Remote shells stay on their owner."),run:async()=>setInventory(record(await operate('merge',enabled?'on':'off')))});}} /></label></>}
    </>}
    {!remoteHost && <LocalDiagnostics key={snapshot.generation} snapshot={snapshot} openDialog={openDialog} />}
    <details className="connection-protocol"><summary>{tr("Protocol details")}</summary>{query.error ? <ErrorBox error={query.error}/> : query.loading ? <Loading/> : <Detail value={{...record(query.data),server_generation:snapshot.generation}}/>}</details>
    <Notice>{tr("Closing the desktop window detaches this client. It does not stop the server or its terminals.")}</Notice></section>;
}

function LocalDiagnostics({snapshot,openDialog}:Props){
  const [rows,setRows]=useState<RecordValue[]>([]),[output,setOutput]=useState<unknown>(),[error,setError]=useState('');
  const generation=snapshot.generation;
  const op=async(action:'list'|'start'|'stop'|'copy',kind:'web'|'uhp',control=false)=>{
    setRows(array(await window.luvus.gateway(action,kind,control,generation)).map(record));
  };
  useEffect(()=>{let active=true;window.luvus.gateway('list','web',false,generation).then(data=>{
    if(active)setRows(array(data).map(record));
  }).catch(e=>{if(active)setError(String(e));});return()=>{active=false;};},[generation]);
  const pane=snapshot.workspaces.find(w=>w.active && !w.host)?.tabs.find(t=>t.active)?.panes.find(p=>p.focused && p.target?.terminal_id);
  return <><h3>{tr("Loopback bridges")}</h3>{error && <ErrorBox error={error} />}
    {rows.map(row=>{const kind=row.kind==='uhp'?'uhp':'web';return <div className="list-card" key={kind} data-bridge={kind}><div><strong>{kind==='web'?'Web':'UHP'}</strong><small>{row.port?`127.0.0.1:${row.port} · ${row.mode}`:row.state === 'stopped' ? tr('Stopped') : text(row.state)}</small></div>
      {row.state==='stopped'?<><AsyncButton run={()=>op('start',kind)}>{tr("Start read-only")}</AsyncButton><button className="button" onClick={()=>openDialog({title:tr("Allow paired control?"),description:tr("A paired client will be able to send input and change workspaces on this session."),submit:tr("Start with control"),run:()=>op('start',kind,true)})}>{tr("Start with control…")}</button></>:<>
      <AsyncButton disabled={!row.port} run={()=>op('copy',kind)}>{tr("Copy pairing")}</AsyncButton><AsyncButton run={()=>op('stop',kind)}>{tr("Stop bridge")}</AsyncButton></>}
    </div>;})}
    <div className="panel-toolbar"><h3>{tr("Diagnostics")}</h3>
      {['server','client'].map(source=><AsyncButton key={source} run={async()=>setOutput(await request({type:'logs',id:crypto.randomUUID(),generation,client:source==='client'}))}>{source==='server'?tr("Server log"):tr("Client log")}</AsyncButton>)}
      <button className="button" disabled={!pane} onClick={()=>openDialog({title:tr("Wait for terminal output"),submit:tr("Wait"),fields:[{key:'needle',label:tr("Match text"),required:true},{key:'timeout',label:tr("Timeout in seconds"),value:'5',required:true}],run:async v=>{
        const timeout=Number(v.timeout);if(!Number.isFinite(timeout)||timeout<=0||timeout>25)throw Error('Use a timeout between 0 and 25 seconds.');
        setOutput(await mutate('wait.output',{pane:pane!.id,match:text(v.needle),timeout_s:timeout},generation));
      }})}>{tr("Wait for output…")}</button></div>
    {output!==undefined && <div data-diagnostic><Detail value={output}/></div>}</>;
}
