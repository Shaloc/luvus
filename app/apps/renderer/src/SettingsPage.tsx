import { tr } from './i18n';
import { useEffect, useRef, useState, type ReactNode } from 'react';
import { ImagePlus, Palette, Trash2, Type } from 'lucide-react';
import { mutate, reportError, useQuery } from './bridge';
import { array, record, text, type Snapshot } from './model';
import { Choice, ErrorBox, IconButton, Loading } from './ui';
export { SettingsPanel } from './SettingsContent';
import { chooseWallpaper, defaultAppearance, setAppearance, terminalFontStack, useAppearance, useWallpaper } from './appearance';

export function Setting({ label, detail, children }: { label: string; detail?: string; children: ReactNode }) {
  return <div className="preference-row"><div><strong>{tr(label)}</strong>{detail && <small>{tr(detail)}</small>}</div><div className="preference-control">{children}</div></div>;
}
export function Toggle({ label, value, save }: { label: string; value: boolean; save: (next: boolean) => Promise<unknown> }) {
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
function AppearanceNumber({field,title,min,max,slider=false}:{field:'uiWeight'|'terminalWeight'|'terminalBoldWeight'|'uiSize'|'terminalSize';title:string;min:number;max:number;slider?:boolean}) {
  const appearance=useAppearance();
  const [draft,setDraft]=useState(String(appearance[field]));
  useEffect(()=>setDraft(String(appearance[field])),[appearance[field]]);
  const valid=Number.isInteger(Number(draft)) && Number(draft)>=min && Number(draft)<=max;
  return <Setting label={title}>{slider && <input aria-label={tr(title)+' '+tr('Slider')} type="range" min={min} max={max} step={1} value={appearance[field]} onChange={e=>setAppearance({[field]:e.target.valueAsNumber})}/>}<input className="size-input" aria-label={tr(title)} aria-invalid={!valid || undefined} type="number" min={min} max={max} step={1} value={draft} onBlur={()=>{if(!valid)setDraft(String(appearance[field]));}} onChange={e=>{setDraft(e.target.value);const value=e.target.valueAsNumber;if(Number.isInteger(value)&&value>=min&&value<=max)setAppearance({[field]:value});}}/></Setting>;
}
export function TerminalAppearance() {
  const value = useAppearance();
  return <section className="settings-section"><h3><Type size={16} />{tr("Text")}</h3><FontSetting terminal /><AppearanceNumber field="terminalSize" title="Terminal font size" min={9} max={32}/><AppearanceNumber field="terminalWeight" title="Terminal font weight" min={100} max={900} slider/><AppearanceNumber field="terminalBoldWeight" title="Terminal bold weight" min={100} max={900} slider/><pre className="font-preview" style={{ fontFamily: terminalFontStack(value.terminalFont), fontSize: value.terminalSize, fontWeight: value.terminalWeight }}>luvus ~/workspace{`\n`}Aa Bb 0123456789{`\n\uf07c src   \ue628 main.ts`}<br/><span style={{fontWeight:value.terminalBoldWeight}}>Aa Bb 0123456789</span></pre></section>;
}
export function AppearancePanel({ snapshot }: { snapshot: Snapshot }) {
  const value = useAppearance(), wallpaper = useWallpaper(), picker = useRef<HTMLInputElement>(null);
  const themes = useQuery('theme.list');
  const [error, setError] = useState(''), [busy, setBusy] = useState(false);
  const image = async (file: File | null) => { setBusy(true); setError(''); try { await chooseWallpaper(file); } catch (e) { setError(String(e)); } finally { setBusy(false); } };
  return <>
    <section className="settings-section"><h3><Palette size={16} />{tr("Theme")}</h3><Setting label={tr("Display mode")}><div className="appearance-modes">{['light', 'dark', 'system'].map(mode => <button key={mode} aria-pressed={mode === value.mode} onClick={() => setAppearance({ mode })}><span className={`mode-preview mode-${mode}`}><i /><b /></span><span>{tr(mode.charAt(0).toUpperCase() + mode.slice(1))}</span></button>)}</div></Setting>
      <Setting label={tr("Terminal theme")}>{themes.loading ? <Loading /> : themes.error ? <ErrorBox error={themes.error} /> : <Choice label={tr("Terminal theme")} value={text(snapshot.config.theme)} options={array(record(themes.data).themes).map(record).map(theme => ({ value: text(theme.id), label: text(theme.display_name) || text(theme.id) }))} onChange={id => void mutate('theme.use', { id }, snapshot.generation).catch(reportError)} />}</Setting>
    </section>
    <section className="settings-section"><h3><Type size={16} />{tr("Interface")}</h3><FontSetting /><AppearanceNumber field="uiSize" title="Interface font size" min={11} max={18}/><AppearanceNumber field="uiWeight" title="Interface font weight" min={100} max={900} slider/></section>
    <section className="settings-section"><h3><ImagePlus size={16} />{tr("Background image")}</h3><div className="wallpaper-picker">{wallpaper ? <img src={wallpaper} alt={tr("Selected background")} /> : <div className="wallpaper-empty"><ImagePlus size={24} /></div>}<div className="button-row"><button className="button" disabled={busy} onClick={() => picker.current?.click()}><ImagePlus size={15} />{tr("Choose image")}</button><IconButton label={tr("Remove background")} disabled={busy || !wallpaper} onClick={() => void image(null)}><Trash2 size={15} /></IconButton></div></div><input ref={picker} type="file" accept="image/png,image/jpeg,image/webp" hidden aria-label={tr("Background image")} onChange={event => { const file = event.target.files?.[0]; if (file) void image(file); event.target.value = ''; }} />{error && <ErrorBox error={error} />}
      <Setting label={tr("Visibility")}><input aria-label={tr("Background visibility")} type="range" min={0} max={100} value={value.wallpaperOpacity} onChange={event => setAppearance({ wallpaperOpacity: event.target.valueAsNumber })} /><output>{value.wallpaperOpacity}%</output></Setting><Setting label={tr("Image fit")}><Choice label={tr("Image fit")} value={value.wallpaperFit} options={[{ value: 'cover', label: tr("Fill") }, { value: 'contain', label: tr("Fit") }]} onChange={wallpaperFit => setAppearance({ wallpaperFit })} /></Setting>
    </section>
  </>;
}
