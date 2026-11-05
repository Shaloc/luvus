import { tr } from './i18n';
import { array, numeric, page, record, text, truth, type RecordValue,type Snapshot } from './model';
import { AsyncButton,Notice } from './ui';
import { request } from './bridge';
import type { Request } from '@luvus/app-protocol';
import type { OpenDialog } from './Actions';

function invoke(snapshot:Snapshot,item:Extract<Request,{type:'widget'}>['item']){
  const workspace=snapshot.workspaces.find(w=>w.active);
  if (!workspace || workspace.host) throw Error('Module action requires the selected local workspace');
  return request({type:'widget',id:crypto.randomUUID(),generation:snapshot.generation,workspace_id:workspace.id,item});
}

export function ProjectedBars({ snapshot }: { snapshot: Snapshot }) {
  const widgets = array(record(snapshot.raw.bars).widgets).map(record);
  return <div className="projected-bars" aria-label={tr("Core bar widgets")}>{widgets.map((widget) => <span className="projected-widget" key={text(widget.id)} title={text(widget.title)}><Segments snapshot={snapshot} widget={widget}/></span>)}</div>;
}
function Segments({snapshot,widget}:{snapshot:Snapshot;widget:RecordValue}){
  const compact=Boolean(array(widget.compact_content).length),items=array(compact?widget.compact_content:widget.content);
  return <>{items.slice(0, 64).map((value, i) => {
    const segment = record(value), tone = text(segment.tone);
    const className = tone === 'success' ? 'added' : tone === 'error' ? 'deleted' : tone === 'muted' ? 'muted' : '';
    if (text(segment.action)) return <AsyncButton key={i} className={'text-button '+className} run={()=>invoke(snapshot,{kind:'bar',owner:text(widget.owner),key:text(widget.id),index:i,action:text(segment.action),value:typeof segment.value==='string'?segment.value:null,compact})}>
      {text(segment.text)||text(segment.label)||text(segment.symbol)||text(segment.state)||text(segment.action)}</AsyncButton>;
    switch (text(segment.type)) {
      case 'text': case 'badge': return <span key={i} className={className}>{text(segment.text)}</span>;
      case 'symbol': return <span key={i} className={className}>{text(segment.symbol)}</span>;
      case 'state': return <span key={i} className={className}>{text(segment.label) || text(segment.state)}</span>;
      case 'separator': return <span key={i} className="muted" aria-hidden="true">|</span>;
      case 'spacer': return <span key={i} style={{ width: `${Math.min(12, numeric(segment.width))}ch` }} />;
      case 'progress': return <progress key={i} value={numeric(segment.value)} max={Math.max(1, numeric(segment.total))} aria-label={tr("Module progress")} />;
      default: return null;
    }
  })}</>;
}
export function ProjectedDocks({ snapshot,openDialog }: { snapshot: Snapshot;openDialog:OpenDialog }) {
  const docks = array(snapshot.raw.docks).flatMap((side) => array(record(side).docks).map(record)).filter((dock) => Array.isArray(record(dock.data).items));
  if (!docks.length) return null;
  return <section className="data-section"><h3>{tr("Module docks")}</h3>{docks.map(dock=><div key={text(dock.id)} className="task-card"><h3>{text(dock.title)}</h3>{page(dock.data).map((row,i)=>{
    const run=(action:string,value:unknown,menu:number|null)=>invoke(snapshot,{kind:'dock',key:text(dock.id),owner:text(dock.owner),
      index:i+numeric(record(dock.data).offset),text:text(row.text),action,value:typeof value==='string'?value:null,menu});
    return <div className="projected-dock-row" key={i}>{dock.owner && row.action?<AsyncButton className="text-button" run={()=>run(text(row.action),row.value,null)}>{text(row.text)}</AsyncButton>:<span>{text(row.text)}</span>}{text(row.value) && <small>{text(row.value)}</small>}
      {page(row.menu).map((it,idx)=>it.action?<button className="text-button" key={idx} onClick={()=>{
        openDialog({title:text(it.title),description:text(row.text),destructive:it.destructive===true,run:()=>run(text(it.action),it.value,idx)});
      }}>{text(it.title)}</button>:null)}</div>;
  })}{truth(record(dock.data).truncated) && <Notice>{tr("Dock rows are truncated by the core budget.")}</Notice>}</div>)}</section>;
}
