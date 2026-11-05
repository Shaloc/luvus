import { tr } from './i18n';
import { useEffect, useRef, useState, type ReactNode } from 'react';
import { Bot, Globe, ImagePlus, Keyboard, Monitor, Palette, Search, Settings2, Terminal, Trash2, Type, Wrench } from 'lucide-react';
import { mutate, reportError, useQuery } from './bridge';
import { type OpenDialog } from './Actions';
import { array, label, record, text, type Snapshot } from './model';
import { AsyncButton, Choice, Detail, ErrorBox, IconButton, Loading } from './ui';
import { LocalAddons, SettingRow, flatten, nest } from './SettingsPanels';
import { chooseWallpaper, defaultAppearance, setAppearance, useAppearance, useWallpaper } from './appearance';

const categories = [
  { id: 'general', label: 'General', icon: Settings2 }, { id: 'appearance', label: 'Appearance', icon: Palette },
  { id: 'terminal', label: 'Terminal', icon: Terminal }, { id: 'workspace', label: 'Workspaces', icon: Monitor },
  { id: 'agents', label: 'Agents', icon: Bot }, { id: 'keys', label: 'Keybindings', icon: Keyboard },
  { id: 'integrations', label: 'Integrations', icon: Wrench }, { id: 'advanced', label: 'Advanced', icon: Settings2 },
];
const languages = [['en', 'English'], ['zh', '简体中文'], ['ja', '日本語'], ['ko', '한국어'], ['de', 'Deutsch'], ['fr', 'Français'], ['pt', 'Português'], ['es', 'Español'], ['id', 'Bahasa Indonesia']].map(([value, label]) => ({ value: value!, label: label! }));
const enums: Record<string, Record<string,string>> = {
  'notifications.display': {inbox:'Inbox only',toast:'Toast'},
  'notifications.sound_style': {retro:'Retro',soft:'Soft',pulse:'Pulse'},
  'commander_working_policy': {ask:'Ask',auto_send:'Send automatically'},
  'layout.workspace_display': {tree:'Tree',flat:'Flat'},
  'layout.file_click': {preview:'Preview',tab:'Tab'},
  'theme_appearance': {auto:'Follow system',light:'Light',dark:'Dark'},
};
function category(path: string[]) {
  const key = path.join('.');
  if (/^(theme|language|keybindings|direct_keybindings|prefix)/.test(key)) return 'advanced';
  if (/^(notifications|check_updates|session)/.test(key)) return 'general';
  if (/^(shell|allow_nested)|^layout\.(shift_enter|scrollback_bytes|new_pane_copy_on_select)/.test(key)) return 'terminal';
  if (/^layout\.agent_|^agents|commander|resume_launch_flags|mission/.test(key)) return 'agents';
  if (/^(layout|worktree|sidebar)/.test(key)) return 'workspace';
  return 'advanced';
}
export function SettingsPanel({ snapshot, openDialog }: { snapshot: Snapshot; openDialog: OpenDialog }) {
  const appearance = useAppearance();
  const [selected, setSelected] = useState('general'), [search, setSearch] = useState('');
  const entries = flatten(snapshot.config).filter(([path]) => !(path.length === 1 && path[0] === 'language')).filter(([path]) => search ? `${tr(label(path.at(-1)!))} ${label(path.join('.'))} ${path.join('.')}`.toLowerCase().includes(search.toLowerCase()) : category(path) === selected);
  const showLanguage = !search ? selected === 'general' : ['Interface language','Core language','Language'].some(s => (s+' '+tr(s)).toLowerCase().includes(search.toLowerCase()));
  const save = (path: string[], next: unknown) => mutate('config.patch', { patch: nest(path, next) }, snapshot.generation);
  return <section className="settings-page">
    <aside className="settings-nav"><div className="filter-input"><Search size={14} /><input aria-label={tr("Find setting")} placeholder={tr("Search settings")} value={search} onChange={event => setSearch(event.target.value)} /></div>
      <nav aria-label={tr("Settings categories")}>{categories.map(item => <button key={item.id} aria-current={!search && item.id === selected ? 'page' : undefined} onClick={() => { setSelected(item.id); setSearch(''); }}><item.icon size={16} />{tr(item.label)}</button>)}</nav>
    </aside>
    <div className="settings-main"><header className="settings-heading"><span>{tr("Settings")}</span><span>/</span><h2>{search ? tr("Search results") : tr(categories.find(item => item.id === selected)?.label ?? '')}</h2></header>
      <div className="settings-content">
        {showLanguage && <section className="settings-section"><h3><Globe size={16} />{tr("Language")}</h3><Setting label={tr("Interface language")}><Choice label={tr("Interface language")} value={appearance.language} options={[{value:'system',label:tr("Follow system")},{value:'en',label:'English'},{value:'zh',label:'简体中文'}]} onChange={language=>setAppearance({language})}/></Setting><Setting label={tr("Core language")}><Choice label={tr("Core language")} value={text(snapshot.config.language, 'en')} options={languages} onChange={value => void save(['language'], value).catch(reportError)} /></Setting></section>}
        {!search && selected === 'appearance' && <AppearancePanel snapshot={snapshot} />}
        {!search && selected === 'terminal' && <TerminalAppearance />}
        {!search && selected === 'integrations' && <LocalAddons generation={snapshot.generation} />}
        {!search && selected === 'keys' && <><h3>{tr("Keyboard shortcuts")}</h3><Detail value={snapshot.raw.key_bindings} /><button className="button" onClick={() => openDialog({ title: tr("Bind a direct shortcut"), fields: [{ key: 'command', label: tr("Command"), kind: 'select', required: true, options: snapshot.commands.map(command => ({ value: text(command.id), label: text(command.label) })) }, { key: 'chord', label: tr("Chord"), required: true }], run: value => save(['direct_keybindings'], { ...record(snapshot.config.direct_keybindings), [text(value.command)]: text(value.chord) }) })}>{tr("Add direct shortcut")}</button></>}
        {!!entries.length && <section className="settings-section"><h3>{search ? tr("Core settings") : tr("Preferences")}</h3>{entries.map(([path, value]) => enums[path.join('.')] ? <Setting key={path.join('.')} label={path.join('.') === 'notifications.display' ? tr("Notification display") : label(path.at(-1)!)}><Choice label={path.join('.')} value={String(value)} options={Object.entries(enums[path.join('.')]!).map(([value,label])=>({value,label:tr(label)}))} onChange={v=>void save(path,v).catch(reportError)}/></Setting> : typeof value === 'boolean' ? <Setting key={path.join('.')} label={label(path.at(-1)!)} detail={path.length > 1 ? label(path.slice(0, -1).join('.')) : undefined}><Toggle label={path.join('.')} value={value} save={next => save(path, next)} /></Setting> : <SettingRow key={`${snapshot.generation}:${path.join('.')}`} path={path} value={value} save={next => save(path, next)} />)}</section>}
        {search && !entries.length && !showLanguage && <p className="muted">{tr("No matching settings")}</p>}
      </div>
    </div>
  </section>;
}
function Setting({ label, detail, children }: { label: string; detail?: string; children: ReactNode }) {
  return <div className="preference-row"><div><strong>{tr(label)}</strong>{detail && <small>{tr(detail)}</small>}</div><div className="preference-control">{children}</div></div>;
}
function Toggle({ label, value, save }: { label: string; value: boolean; save: (next: boolean) => Promise<unknown> }) {
  const [busy, setBusy] = useState(false);
  return <button type="button" role="switch" className="toggle" aria-label={label} aria-checked={value} disabled={busy} onClick={() => { setBusy(true); void save(!value).catch(reportError).finally(() => setBusy(false)); }}><span /></button>;
}
function FontSetting({ terminal = false }: { terminal?: boolean }) {
  const appearance = useAppearance(), key = terminal ? 'terminalFont' : 'uiFont';
  const presets = terminal ? [
    { value: defaultAppearance.terminalFont, label: tr("System monospace") },
    { value: 'Menlo, monospace', label: 'Menlo' }, { value: 'Monaco, monospace', label: 'Monaco' },
    { value: '"JetBrains Mono", monospace', label: 'JetBrains Mono' }, { value: '"SF Mono", monospace', label: 'SF Mono' },
  ] : [
    { value: defaultAppearance.uiFont, label: tr("System font") }, { value: 'Inter, sans-serif', label: 'Inter' },
    { value: 'Helvetica, sans-serif', label: 'Helvetica' }, { value: '"PingFang SC", sans-serif', label: 'PingFang SC' },
  ];
  const [installed, setInstalled] = useState<string[]>([]);
  useEffect(() => {
    let live = true;
    const query = (window as Window & {queryLocalFonts?: () => Promise<{family:string}[]>}).queryLocalFonts;
    if (query) void query.call(window).then(fonts => {
      const context = document.createElement('canvas').getContext('2d');
      const families = [...new Set(fonts.map(f=>f.family))].filter(f=> !/["\\\\\x00-\x1f]/.test(f) && f.length <= 128 && f.length > 0);
      const choices = families.filter(f => {
        if (!terminal || !context) return true;
        context.font = `14px "${f}"`;
        return Math.abs(context.measureText('iiii').width - context.measureText('WWWW').width) < .01;
      }).sort((a,b)=>a.localeCompare(b));
      if (live) setInstalled(choices);
    }).catch(()=>{});
    return ()=>{live=false;};
  },[terminal]);
  const title = terminal ? tr("Terminal font") : tr("Interface font");
  const options = [...presets.filter((p,i)=> !installed.length || !i || installed.includes(p.label)), ...installed.filter(f=>!presets.some(p=>p.label===f)).map(f=>({label:f,value:`"${f}", ${terminal ? 'monospace' : 'sans-serif'}`}))];
  if (!options.some(option=>option.value === appearance[key])) options.push({value:appearance[key],label:appearance[key].split(',')[0]!.replaceAll('"','')});
  return <Setting label={title}><Choice label={title} value={appearance[key]} options={options} onChange={v=>setAppearance({[key]:v})}/></Setting>;
}
function TerminalAppearance() {
  const value = useAppearance();
  return <section className="settings-section"><h3><Type size={16} />{tr("Text")}</h3><FontSetting terminal /><Setting label={tr("Terminal font size")}><input className="size-input" aria-label={tr("Terminal font size")} type="number" min={9} max={32} value={value.terminalSize} onChange={event => { const n = event.target.valueAsNumber; if (n >= 9 && n <= 32) setAppearance({ terminalSize: n }); }} /></Setting><pre className="font-preview" style={{ fontFamily: value.terminalFont, fontSize: value.terminalSize }}>luvus ~/workspace{`\n`}Aa Bb 0123456789</pre></section>;
}
function AppearancePanel({ snapshot }: { snapshot: Snapshot }) {
  const value = useAppearance(), wallpaper = useWallpaper(), picker = useRef<HTMLInputElement>(null);
  const themes = useQuery('theme.list');
  const [error, setError] = useState(''), [busy, setBusy] = useState(false);
  const image = async (file: File | null) => { setBusy(true); setError(''); try { await chooseWallpaper(file); } catch (e) { setError(String(e)); } finally { setBusy(false); } };
  return <>
    <section className="settings-section"><h3><Palette size={16} />{tr("Theme")}</h3><Setting label={tr("Display mode")}><div className="appearance-modes">{['light', 'dark', 'system'].map(mode => <button key={mode} aria-pressed={mode === value.mode} onClick={() => setAppearance({ mode })}><span className={`mode-preview mode-${mode}`}><i /><b /></span><span>{tr(mode.charAt(0).toUpperCase() + mode.slice(1))}</span></button>)}</div></Setting>
      <Setting label={tr("Terminal theme")}>{themes.loading ? <Loading /> : themes.error ? <ErrorBox error={themes.error} /> : <Choice label={tr("Terminal theme")} value={text(snapshot.config.theme)} options={array(record(themes.data).themes).map(record).map(theme => ({ value: text(theme.id), label: text(theme.display_name) || text(theme.id) }))} onChange={id => void mutate('theme.use', { id }, snapshot.generation).catch(reportError)} />}</Setting>
    </section>
    <section className="settings-section"><h3><Type size={16} />{tr("Interface")}</h3><FontSetting /><Setting label={tr("Interface font size")}><input className="size-input" aria-label={tr("Interface font size")} type="number" min={11} max={18} value={value.uiSize} onChange={event => { const n = event.target.valueAsNumber; if (n >= 11 && n <= 18) setAppearance({ uiSize: n }); }} /></Setting></section>
    <section className="settings-section"><h3><ImagePlus size={16} />{tr("Background image")}</h3><div className="wallpaper-picker">{wallpaper ? <img src={wallpaper} alt={tr("Selected background")} /> : <div className="wallpaper-empty"><ImagePlus size={24} /></div>}<div className="button-row"><button className="button" disabled={busy} onClick={() => picker.current?.click()}><ImagePlus size={15} />{tr("Choose image")}</button><IconButton label={tr("Remove background")} disabled={busy || !wallpaper} onClick={() => void image(null)}><Trash2 size={15} /></IconButton></div></div><input ref={picker} type="file" accept="image/png,image/jpeg,image/webp" hidden aria-label={tr("Background image")} onChange={event => { const file = event.target.files?.[0]; if (file) void image(file); event.target.value = ''; }} />{error && <ErrorBox error={error} />}
      <Setting label={tr("Visibility")}><input aria-label={tr("Background visibility")} type="range" min={0} max={100} value={value.wallpaperOpacity} onChange={event => setAppearance({ wallpaperOpacity: event.target.valueAsNumber })} /><output>{value.wallpaperOpacity}%</output></Setting><Setting label={tr("Image fit")}><Choice label={tr("Image fit")} value={value.wallpaperFit} options={[{ value: 'cover', label: tr("Fill") }, { value: 'contain', label: tr("Fit") }]} onChange={wallpaperFit => setAppearance({ wallpaperFit })} /></Setting>
    </section>
  </>;
}
