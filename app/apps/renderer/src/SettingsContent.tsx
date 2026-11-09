import { useEffect, useState } from 'react';
import { Bot, FileDiff, Globe, Keyboard, Monitor, Palette, Search, Server, Settings2, Terminal, Wrench } from 'lucide-react';
import { type OpenDialog } from './Actions';
import { action, mutate, reportError, useQuery } from './bridge';
import { record, text, type Snapshot } from './model';
import { Choice } from './ui';
import { tr } from './i18n';
import { LocalAddons, nest } from './SettingsPanels';
import { AppearancePanel, Setting, TerminalAppearance, Toggle } from './SettingsPage';
import { setAppearance, useAppearance } from './appearance';
import { settingSpecs, settingValue, type SettingSpec } from './settings-schema';
import { BarSettings, HostSettings, KeySettings, PricingSettings, ValueInput } from './StructuredSettings';

const categories = [
  { id: 'general', title: 'General', icon: Settings2 }, { id: 'appearance', title: 'Appearance', icon: Palette },
  { id: 'terminal', title: 'Terminal', icon: Terminal }, { id: 'workspace', title: 'Workspaces', icon: Monitor },
  { id: 'files', title: 'Files and diffs', icon: FileDiff }, { id: 'agents', title: 'Agents', icon: Bot },
  { id: 'keys', title: 'Keybindings', icon: Keyboard }, { id: 'connections', title: 'Connections', icon: Server },
  { id: 'integrations', title: 'Integrations', icon: Wrench }, { id: 'advanced', title: 'Layout and bars', icon: Monitor },
];
const languages = [['en','English'],['zh','简体中文'],['ja','日本語'],['ko','한국어'],['de','Deutsch'],['fr','Français'],['pt','Português'],['es','Español'],['id','Bahasa Indonesia']].map(([value,label])=>({value:value!,label:label!}));

export function SettingsPanel({snapshot}: {snapshot: Snapshot; openDialog: OpenDialog}) {
  const appearance = useAppearance();
  const [selected,setSelected] = useState('general'), [search,setSearch] = useState('');
  const match = (...values:string[]) => values.some(value=>(value+' '+tr(value)).toLowerCase().includes(search.trim().toLowerCase()));
  const visible = (category:string,...terms:string[]) => search.trim() ? match(...terms) : selected===category;
  const specs = settingSpecs.filter(spec=>settingValue(snapshot.config,spec.path)!==undefined && (search.trim() ? match(spec.title,spec.group,spec.path) : spec.category===selected));
  const groups = [...new Set(specs.map(spec=>spec.group))];
  const panels = {
    language: visible('general','Language','Interface language','Core language'),
    confirmations: visible('general','Confirm before closing a tab'),
    appearance: visible('appearance','Appearance','Theme','Interface font','Interface font size','Interface font weight','Background image'),
    terminal: visible('terminal','Terminal font','Terminal font size','Terminal font weight','Terminal bold weight'),
    grouping: visible('agents','Agent grouping','agents_group_by_workspace','agents_group_by_date'),
    keys: visible('keys','Keybindings','Keyboard shortcuts','Prefix','direct_keybindings',...snapshot.commands.map(command=>text(command.label))),
    hosts: visible('connections','Remote hosts','Registered SSH hosts','remote_hosts'),
    pricing: visible('agents','Usage and pricing','Workspace cost budget','Budget (USD)','mission_budget','mission_pricing'),
    bars: visible('advanced','Status bars','bars','docks_off','TUI sidebars','TUI dock placement'),
    integrations: visible('integrations','Integrations','Bundled skill'),
  };
  const save = (path:string,next:unknown) => mutate('config.patch',{patch:nest(path.split('.'),next)},snapshot.generation);
  return <section className="settings-page">
    <aside className="settings-nav"><div className="filter-input"><Search size={14}/><input aria-label={tr('Find setting')} placeholder={tr('Search settings')} value={search} onChange={e=>setSearch(e.target.value)}/></div>
      <nav aria-label={tr('Settings categories')}>{categories.map(item=><button key={item.id} aria-current={!search && selected===item.id?'page':undefined} onClick={()=>{setSelected(item.id);setSearch('');}}><item.icon size={15}/>{tr(item.title)}</button>)}</nav>
    </aside>
    <div className="settings-main" key={selected}><header className="settings-heading"><span>{tr('Settings')}</span><span>/</span><h2>{tr(search?'Search results':categories.find(c=>c.id===selected)!.title)}</h2></header>
      <div className="settings-content">
        {search.trim() && !specs.length && !Object.values(panels).some(Boolean) && <p className="empty-state">{tr('No matching settings')}</p>}
        {panels.language && <section className="settings-section"><h3><Globe size={15}/>{tr('Language')}</h3>
          <Setting label="Interface language"><Choice label={tr('Interface language')} value={appearance.language} options={[{value:'system',label:tr('Follow system')},{value:'en',label:'English'},{value:'zh',label:'简体中文'}]} onChange={language=>setAppearance({language})}/></Setting>
          <Setting label="Core language"><Choice label={tr('Core language')} value={text(snapshot.config.language,'en')} options={languages} onChange={v=>void save('language',v).catch(reportError)}/></Setting>
        </section>}
        {panels.confirmations && <section className="settings-section"><h3>{tr('Confirmations')}</h3><Setting label="Confirm before closing a tab"><Toggle label={tr('Confirm before closing a tab')} value={appearance.confirmTabClose} save={async confirmTabClose=>setAppearance({confirmTabClose})}/></Setting></section>}
        {panels.appearance && <AppearancePanel snapshot={snapshot}/>}
        {panels.terminal && <TerminalAppearance/>}
        {panels.grouping && <section className="settings-section"><h3>{tr('Organization')}</h3><Setting label="Agent grouping"><Choice label={tr('Agent grouping')} value={snapshot.config.agents_group_by_workspace?'workspace':snapshot.config.agents_group_by_date?'date':'none'} options={[{value:'none',label:tr('No grouping')},{value:'workspace',label:tr('Workspace')},{value:'date',label:tr('Date')}]} onChange={value=>void mutate('config.patch',{patch:{agents_group_by_workspace:value==='workspace',agents_group_by_date:value==='date'}},snapshot.generation).catch(reportError)}/></Setting></section>}
        {groups.map(group=><section className="settings-section" key={group} data-settings-group={group}><h3>{tr(group)}</h3>{specs.filter(spec=>spec.group===group).map(spec=><CoreSetting key={snapshot.generation+spec.path} spec={spec} value={settingValue(snapshot.config,spec.path)} save={value=>save(spec.path,value)}/>)}</section>)}
        {panels.keys && <KeySettings snapshot={snapshot} save={save}/>}
        {panels.hosts && <HostSettings snapshot={snapshot} save={save}/>}
        {panels.pricing && <PricingSettings snapshot={snapshot} save={save}/>}
        {panels.bars && <BarSettings snapshot={snapshot} save={save}/>}
        {panels.integrations && <LocalAddons generation={snapshot.generation}/>}
      </div>
    </div>
  </section>;
}

function CoreSetting({spec,value,save}: {spec:SettingSpec;value:unknown;save:(next:unknown)=>Promise<unknown>}) {
  let control;
  if (spec.kind==='toggle') control=<Toggle label={spec.path} value={value===true} save={save}/>;
  else if (spec.kind==='choice') control=<Choice label={spec.path} value={String(value)} options={Object.entries(spec.options!).map(([value,label])=>({value,label:tr(label)}))} onChange={v=>void save(v).catch(reportError)}/>;
  else if (spec.kind==='editor') control=<EditorChoice value={String(value)} save={save}/>;
  else if (spec.kind==='provider') control=<ProviderChoice value={String(value)} save={save}/>;
  else control=<ValueInput spec={spec} value={value} save={save}/>;
  return <Setting label={spec.title}>{control}</Setting>;
}
function EditorChoice({value,save}:{value:string;save:(next:unknown)=>Promise<unknown>}) {
  const presets={readonly:tr('Built-in viewer'),nvim:'Neovim',vim:'Vim',nano:'Nano','emacs -nw':'Emacs'};
  const [custom,setCustom]=useState(!Object.hasOwn(presets,value));
  return <div className="setting-stacked-control"><Choice label="layout.file_open" value={custom?'custom':value} options={[...Object.entries(presets).map(([value,label])=>({value,label})),{value:'custom',label:tr('Custom editor')}]} onChange={v=>{setCustom(v==='custom');if(v!=='custom')void save(v).catch(reportError);}}/>{custom && <ValueInput spec={{path:'editor-command',title:'Editor command',category:'files',group:'File browser',kind:'text'}} value={value} save={save}/>}</div>;
}
function ProviderChoice({value,save}:{value:string;save:(next:unknown)=>Promise<unknown>}) {
  const providers=useQuery('module.list');
  const [modules,setModules]=useState<{value:string;label:string}[]>([]);
  useEffect(()=>{
    let live=true;
    const rows=record(providers.data).modules;
    if(Array.isArray(rows))void (async()=>{
      const options=[];
      for(const item of rows.slice(0,64).map(record).filter(m=>m.enabled)) {
        if(!live)return;
        const info=record(await action('module.info',{id:item.id}));
        if(info.worktree_provider)options.push({value:text(item.id),label:text(item.name)||text(item.id)});
      }
      if(live)setModules(options);
    })().catch(reportError);
    return()=>{live=false;};
  },[providers.data]);
  const options=[{value:'git',label:'Git'},...modules];
  if(!options.some(o=>o.value===value))options.push({value,label:value});
  return <Choice label="worktree.provider" value={value} options={options} onChange={v=>void save(v).catch(reportError)}/>;
}
