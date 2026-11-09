import { useEffect, useRef, useState, type KeyboardEvent } from 'react';
import { ArrowDown, ArrowUp, Plus, RotateCcw, Trash2 } from 'lucide-react';
import { tr } from './i18n';
import { array, checkedDomainResult, record, text, type Snapshot } from './model';
import { mutate, reportError, useQuery } from './bridge';
import { AsyncButton, Choice, ErrorBox, Loading } from './ui';
import { Setting, Toggle } from './SettingsPage';
import { bindingPatch, recordedBinding } from './keyboard';
import { numericSetting, type SettingSpec } from './settings-schema';

type Save = (path:string,value:unknown)=>Promise<unknown>;
type Props = {snapshot:Snapshot;save:Save};
const priceColumns = ['Input (USD/M tokens)', 'Output (USD/M tokens)', 'Cached (USD/M tokens)'];

export function ValueInput({spec,value,save}: {spec:SettingSpec;value:unknown;save:(next:unknown)=>Promise<unknown>}) {
  const incoming=String(typeof value==='number'?value/(spec.scale??1):value??'');
  const [draft,setDraft]=useState(incoming),previous=useRef(incoming);
  useEffect(()=>{const old=previous.current;previous.current=incoming;setDraft(current=>current===old?incoming:current);},[incoming]);
  const numeric=spec.kind==='number';
  const valid=numeric?draft.trim()!=='' && Number.isInteger(Number(draft)) && Number(draft)>=spec.min! && Number(draft)<=spec.max!:true;
  return <form className={`setting-input ${numeric?'setting-number':''}`} onSubmit={e=>e.preventDefault()}>
    <input aria-label={spec.path} aria-invalid={!valid || undefined} type={numeric?'number':'text'} min={spec.min} max={spec.max} step={numeric?1:undefined} value={draft} onChange={e=>setDraft(e.target.value)}/>
    {spec.unit && <span className="input-unit">{tr(spec.unit)}</span>}
    <AsyncButton type="submit" disabled={!valid || draft===incoming} run={()=>save(numeric?numericSetting(draft,spec):draft)}>{tr('Save')}</AsyncButton>
  </form>;
}

export function HostSettings({snapshot,save}: Props) {
  const hosts=array(snapshot.config.remote_hosts).map(String),[draft,setDraft]=useState('');
  const [pending,setPending]=useState(false),inFlight=useRef(false);
  const change=async(next:string[])=>{
    if(inFlight.current)return;
    inFlight.current=true;setPending(true);
    try {await save('remote_hosts',next);} finally {inFlight.current=false;setPending(false);}
  };
  const valid=/^[a-zA-Z0-9_][a-zA-Z0-9_.-]*$/.test(draft) && !hosts.includes(draft);
  return <section className="settings-section"><h3>{tr('Registered SSH hosts')}</h3>
    {hosts.map(host=><div className="settings-list-row" key={host}><span>{host}</span><AsyncButton disabled={pending} aria-label={`${tr('Remove host')} ${host}`} run={()=>change(hosts.filter(h=>h!==host))}><Trash2 size={14}/></AsyncButton></div>)}
    <form className="settings-add-row" onSubmit={e=>e.preventDefault()}><input aria-label={tr('SSH alias')} placeholder={tr('SSH alias')} value={draft} disabled={pending} onChange={e=>setDraft(e.target.value)}/><AsyncButton type="submit" disabled={!valid || pending} run={async()=>{await change([...hosts,draft]);setDraft('');}}><Plus size={14}/>{tr('Add host')}</AsyncButton></form>
  </section>;
}

function KeyRecorder({value,label,mode,save}: {value:string;label:string;mode:'prefix'|'key'|'direct';save:(next:string)=>Promise<unknown>}) {
  const [recording,setRecording]=useState(false),[error,setError]=useState(''),[busy,setBusy]=useState(false);
  const capture=(event:KeyboardEvent<HTMLInputElement>)=>{
    event.preventDefault();event.stopPropagation();
    if(event.key==='Escape' && !event.ctrlKey && !event.altKey && !event.shiftKey && !event.metaKey){setRecording(false);setError('');return;}
    if(['Control','Shift','Alt','Meta'].includes(event.key))return;
    const next=recordedBinding(event,mode);
    if(next===undefined){setError(tr(mode==='key'?'Use a character, arrow key, Tab or Enter.':mode==='prefix'?'Use Ctrl or Alt with a character, or F1–F12.':'Use a supported key with Ctrl, Alt or Shift.'));return;}
    setBusy(true);setError('');
    void save(next).then(()=>setRecording(false)).catch(e=>setError(String(e))).finally(()=>setBusy(false));
  };
  return <div className="key-recorder">{recording?<input autoFocus readOnly data-key-recorder aria-label={label} value={tr('Press a key…')} disabled={busy} onKeyDown={capture} onBlur={()=>setRecording(false)}/>:<button className="button key-binding" aria-label={label} onClick={()=>setRecording(true)}><kbd>{value || tr('Unassigned')}</kbd></button>}{error && <small role="alert">{error}</small>}</div>;
}
export function KeySettings({snapshot,save}:Props) {
  const [mode,setMode]=useState<'key'|'direct'>('key');
  const field=mode==='key'?'keybindings':'direct_keybindings',overrides=record(snapshot.config[field]);
  const effective=record(record(snapshot.raw.key_bindings)[mode==='key'?'prefix_commands':'direct_commands']);
  return <section className="settings-section"><h3>{tr('Keyboard shortcuts')}</h3>
    <Setting label="Prefix"><KeyRecorder label="prefix" value={text(snapshot.config.prefix)} mode="prefix" save={v=>save('prefix',v)}/></Setting>
    <div className="segmented settings-tabs" role="tablist" aria-label={tr('Shortcut type')}>{[['key','Prefix shortcuts'],['direct','Direct shortcuts']].map(([id,title])=><button key={id} role="tab" aria-selected={id===mode} onClick={()=>setMode(id as typeof mode)}>{tr(title!)}</button>)}</div>
    <div className="shortcut-table">{snapshot.commands.map(command=>{
      const id=text(command.id),name=tr(text(command.label)),keys=Object.entries(effective).filter(([,cmd])=>cmd===id).map(([key])=>key).join(' / ');
      return <div className="settings-list-row" key={id}><span>{name}</span><KeyRecorder label={`${name} ${tr(mode==='key'?'Prefix shortcuts':'Direct shortcuts')}`} value={keys} mode={mode} save={key=>save(field,bindingPatch(overrides,id,key,mode==='direct'))}/><AsyncButton aria-label={`${tr('Clear shortcut')} ${name}`} disabled={!keys} run={()=>save(field,{[id]:''})}><Trash2 size={13}/></AsyncButton><AsyncButton aria-label={`${tr('Restore default')} ${name}`} disabled={!Object.hasOwn(overrides,id)} run={()=>save(field,{[id]:null})}><RotateCcw size={13}/></AsyncButton></div>;
    })}</div>
  </section>;
}

export function PricingSettings({snapshot,save}:Props) {
  const budget=snapshot.config.mission_budget,prices=record(snapshot.config.mission_pricing);
  const [model,setModel]=useState('');
  return <section className="settings-section"><h3>{tr('Usage and pricing')}</h3>
    <Setting label="Workspace cost budget"><Toggle label="mission-budget-enabled" value={typeof budget==='number'} save={enabled=>save('mission_budget',enabled?100:null)}/></Setting>
    {typeof budget==='number' && <Setting label="Budget (USD)"><DecimalInput label="mission_budget" value={budget} save={v=>save('mission_budget',v)}/></Setting>}
    <div className="price-table"><div className="price-heading"><span>{tr('Model')}</span>{priceColumns.map(label=><span key={label}>{tr(label)}</span>)}<span/></div>
      {Object.entries(prices).map(([id,prices])=><PriceRow key={id} id={id} values={array(prices).map(Number)} save={v=>save('mission_pricing',{[id]:v})}/>)}</div>
    <form className="settings-add-row" onSubmit={e=>e.preventDefault()}><input aria-label={tr('Model name')} placeholder={tr('Model name')} value={model} onChange={e=>setModel(e.target.value)}/><AsyncButton type="submit" disabled={!model.trim() || Object.hasOwn(prices,model.trim())} run={async()=>{await save('mission_pricing',{[model.trim()]:[0,0,0]});setModel('');}}><Plus size={14}/>{tr('Add pricing')}</AsyncButton></form>
  </section>;
}
function DecimalInput({label,value,save}: {label:string;value:number;save:(next:number)=>Promise<unknown>}) {
  const [draft,setDraft]=useState(String(value));useEffect(()=>setDraft(String(value)),[value]);
  return <form className="setting-input setting-number" onSubmit={e=>e.preventDefault()}><input aria-label={label} type="number" min={0} step="any" value={draft} onChange={e=>setDraft(e.target.value)}/><AsyncButton type="submit" disabled={!draft.trim() || !Number.isFinite(Number(draft)) || Number(draft)<0 || Number(draft)===value} run={()=>save(Number(draft))}>{tr('Save')}</AsyncButton></form>;
}
function PriceRow({id,values,save}: {id:string;values:number[];save:(next:number[]|null)=>Promise<unknown>}) {
  const incoming=JSON.stringify(values),[draft,setDraft]=useState(values.map(String));useEffect(()=>setDraft(JSON.parse(incoming).map(String)),[incoming]);
  return <div className="price-row"><span title={id}>{id}</span>{priceColumns.map((name,i)=><input key={name} aria-label={`${id} ${tr(name)}`} type="number" min={0} step="any" value={draft[i]??''} onChange={e=>setDraft(d=>d.map((v,j)=>j===i?e.target.value:v))}/>)}<div className="button-row"><AsyncButton disabled={draft.some(v=>!v.trim() || !Number.isFinite(Number(v)) || Number(v)<0) || JSON.stringify(draft.map(Number))===incoming} run={()=>save(draft.map(Number))}>{tr('Save')}</AsyncButton><AsyncButton aria-label={`${tr('Remove pricing')} ${id}`} run={()=>save(null)}><Trash2 size={14}/></AsyncButton></div></div>;
}

export function BarSettings({snapshot,save}:Props) {
  const query=useQuery('ui.bar.list',{},true,true),docks=useQuery('ui.dock.list',{},true,true);
  const off=array(snapshot.config.docks_off).map(String),bars=record(snapshot.config.bars);
  const rows=array(record(query.data).widgets).map(record).sort((a,b)=>{
    const regions=['top-right','bottom-right','off'],ar=text(a.region,'off'),br=text(b.region,'off');
    if(ar!==br)return regions.indexOf(ar)-regions.indexOf(br);
    const order=array(bars[ar==='top-right'?'top_right':ar==='bottom-right'?'bottom_right':'off']).map(String);
    const rank=(row:Record<string,unknown>)=>{const index=order.indexOf(text(row.key));return index<0?10000:index;};
    return rank(a)-rank(b);
  });
  const dockRows=array(record(docks.data).docks).map(record);
  const dockNames=[...new Set(['workspaces','agents','files',...dockRows.map(row=>text(row.id)),...off])];
  const moveDock=async(id:string,side:string)=>{
    if(side==='off') {
      const sidebars=record(snapshot.config.sidebars),patch:Record<string,unknown>={};
      for(const side of ['left','right']) {
        const cfg=record(sidebars[side]);
        const ids=Array.isArray(cfg.docks)?cfg.docks.map(String):dockRows.filter(row=>row.side===side).map(row=>text(row.id));
        const weights=Array.isArray(cfg.dock_weights)?cfg.dock_weights:ids.map(()=>1);
        patch[side]={docks:ids.filter(key=>key!==id),dock_weights:weights.filter((_,i)=>ids[i]!==id)};
      }
      await mutate('config.patch',{patch:{sidebars:patch,...(!['workspaces','agents','files'].includes(id)?{docks_off:[...new Set([...off,id])]}:{})}},snapshot.generation);
    }else checkedDomainResult('ui.dock.move',await mutate('ui.dock.move',{id,side},snapshot.generation));
    docks.reload();
  };
  const move=async(row:Record<string,unknown>,region:string)=>{await mutate('ui.bar.move',{id:row.id,owner:row.owner,region},snapshot.generation);query.reload();};
  const order=async(key:string,region:string,direction:number)=>{
    const field=region==='top-right'?'top_right':'bottom_right';
    const explicit=array(bars[field]).map(String),visible=rows.filter(row=>row.region===region).map(row=>text(row.key));
    const keys=[...new Set([...explicit,...visible])],index=keys.indexOf(key),next=index+direction;
    if(index<0 || next<0 || next>=keys.length)return;
    [keys[index],keys[next]]=[keys[next]!,keys[index]!];await save(`bars.${field}`,keys);query.reload();
  };
  return <>
    <section className="settings-section"><h3>{tr('Status bars')}</h3>{query.error?<ErrorBox error={query.error}/>:query.loading?<Loading/>:rows.map(row=>{
      const key=text(row.key),region=text(row.region,'off');
      const orderKeys=[...new Set([...array(bars[region==='top-right'?'top_right':'bottom_right']).map(String),...rows.filter(r=>r.region===region).map(r=>text(r.key))])],index=orderKeys.indexOf(key);
      return <div className="settings-list-row" key={key}><span>{tr(text(row.title)||key)}</span><Choice label={key} value={region} options={[{value:'top-right',label:tr('Top right')},{value:'bottom-right',label:tr('Bottom right')},{value:'off',label:tr('Hidden')}]} onChange={v=>void move(row,v).catch(reportError)}/><AsyncButton aria-label={`${tr('Move up')} ${key}`} disabled={region==='off'||index<=0} run={()=>order(key,region,-1)}><ArrowUp size={14}/></AsyncButton><AsyncButton aria-label={`${tr('Move down')} ${key}`} disabled={region==='off'||index===orderKeys.length-1} run={()=>order(key,region,1)}><ArrowDown size={14}/></AsyncButton></div>;
    })}</section>
    <section className="settings-section"><h3>{tr('TUI dock placement')}</h3>{docks.error?<ErrorBox error={docks.error}/>:docks.loading?<Loading/>:dockNames.map(id=><Setting key={id} label={id}><Choice label={`dock-${id}`} value={text(dockRows.find(r=>r.id===id)?.side,'off')} options={[{value:'left',label:tr('Left')},{value:'right',label:tr('Right')},{value:'off',label:tr('Hidden')}]} onChange={side=>void moveDock(id,side).catch(reportError)}/></Setting>)}
    {['left','right'].flatMap(side=>{
      const cfg=record(record(snapshot.config.sidebars)[side]),weights=array(cfg.dock_weights).map(Number),ids=array(cfg.docks).map(String);
      return ids.map((id,i)=>weights[i]===undefined?null:<Setting key={side+id} label={`${tr(side==='left'?'Left':'Right')} · ${tr(id)}`} detail={tr('Dock height ratio')}><ValueInput spec={{path:`sidebars.${side}.dock_weights.${i}`,kind:'number',min:1,max:65535,title:'Dock height ratio',category:'advanced',group:'TUI sidebars'}} value={weights[i]} save={v=>save(`sidebars.${side}.dock_weights`,weights.map((w,j)=>i===j?v:w))}/></Setting>);
    })}</section>
  </>;
}
