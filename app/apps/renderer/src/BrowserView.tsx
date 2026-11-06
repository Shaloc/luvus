import { tr } from './i18n';
import { useEffect, useRef, useState } from 'react';
import { ArrowLeft, ArrowRight, Globe, MousePointer2, RefreshCw } from 'lucide-react';
import { request, reportError } from './bridge';
import { record, targetKey, text, type Pane } from './model';
import { AsyncButton, Empty, IconButton } from './ui';

function guestBounds(surface: HTMLElement | null) {
  if (!surface || !surface.isConnected || document.querySelector('dialog[open]')) return null;
  const {left,top,right,bottom} = surface.getBoundingClientRect();
  const x = Math.max(0,left), y = Math.max(0,top);
  const r = Math.min(innerWidth,right), b = Math.min(innerHeight,bottom);
  if (r <= x || b <= y) return null;
  for (const overlay of document.querySelectorAll('[data-app-overlay], details.dropdown[open] > .dropdown-menu')) {
    const rect = overlay.getBoundingClientRect();
    if (rect.width && rect.height && rect.left < r && rect.right > x && rect.top < b && rect.bottom > y) return null;
  }
  return {x,y,width:r-x,height:b-y};
}

const opened = new Set<string>();
export function rememberBrowser(generation: string, pane: string) {
  opened.add(`${generation}:${pane}`);
  while (opened.size > 128) opened.delete(opened.values().next().value!);
  window.dispatchEvent(new Event('luvus-browser-created'));
}

export function BrowserView({ pane }: { pane: Pane }) {
  const [browser, setBrowser] = useState<string | null>(null);
  const [state, setState] = useState('closed');
  const [controlled, setControlled] = useState(false);
  const address = (value: string) => value === 'about:blank' ? '' : value;
  const [url, setUrl] = useState('');
  const surface = useRef<HTMLDivElement>(null), addressBar = useRef<HTMLInputElement>(null);
  useEffect(() => { if (state === 'ready' && !url) addressBar.current?.focus(); }, [state, pane.id]);
  const identity = pane.target ? targetKey(pane.target) : pane.id;
  const currentIdentity = useRef(identity);
  const opening = useRef(false);
  currentIdentity.current = identity;
  useEffect(() => { setBrowser(null); setState('closed'); setControlled(false); setUrl(''); }, [identity]);
  const editing = useRef(false);
  const apply = (value: unknown) => {
    const data = record(value);
    if (data.browser_id !== browser) return;
    setState(text(data.state, 'closed'));
    setControlled(data.controlled === true);
    if (typeof data.url === 'string' && !editing.current) setUrl(address(data.url));
  };
  useEffect(() => {
    if (!browser) return;
    const off = window.luvus.onMessage(message => {
      if (message.type === 'effect' && message.kind === 'browser_status') apply(message.data);
    });
    void window.luvus.browserStatus(browser).then(apply).catch(reportError);
    return off;
  }, [browser]);
  useEffect(() => {
    if (!browser || !surface.current) return;
    let frame = 0;
    const place = () => {
      frame = 0;
      const bounds = state === 'ready' ? guestBounds(surface.current) : null;
      void window.luvus.browserBounds(browser, bounds).catch(reportError);
    };
    const schedule = () => { if (!frame) frame = requestAnimationFrame(place); };
    const resize = new ResizeObserver(schedule);
    resize.observe(surface.current);
    const overlays = new MutationObserver(schedule);
    overlays.observe(document.body, { attributes: true, childList: true, subtree: true, attributeFilter: ['open'] });
    window.addEventListener('resize', schedule);
    place();
    return () => {
      cancelAnimationFrame(frame); resize.disconnect(); overlays.disconnect();
      window.removeEventListener('resize', schedule);
      void window.luvus.browserBounds(browser, null).catch(() => {});
    };
  }, [browser, state]);
  const open = async () => {
    if (!pane.target) throw new Error(tr("Browser target is unavailable"));
    if (opening.current) return;
    opening.current = true;
    setState('preparing');
    try {
      const result = record(await request({ type: 'browser_open', id: crypto.randomUUID(), target: pane.target }));
      if (currentIdentity.current !== identity) return;
      const id = text(result.browser_id);
      if (!id) throw new Error(tr("Core did not return a browser identity"));
      rememberBrowser(pane.target.generation, pane.id);
      setBrowser(id);
    } catch (error) { if (currentIdentity.current === identity) { setState('failed'); throw error; } }
    finally { opening.current = false; }
  };
  useEffect(() => {
    const resume = () => { if (pane.target && opened.has(`${pane.target.generation}:${pane.id}`)) void open().catch(reportError); };
    currentIdentity.current = identity;
    resume(); window.addEventListener('luvus-browser-created', resume);
    return () => { currentIdentity.current = ''; window.removeEventListener('luvus-browser-created', resume); };
  }, [identity]);
  const navigate = async (action: 'navigate' | 'back' | 'forward' | 'reload') => {
    const bounds = guestBounds(surface.current);
    if (!browser || !bounds || state !== 'ready') return;
    if (currentIdentity.current !== identity) return;
    editing.current = false;
    try { await window.luvus.browserNavigate(browser, action, bounds, action === 'navigate' ? url : undefined); }
    catch (error) { if (currentIdentity.current === identity) throw error; }
  };
  return <div className="browser-view">
    <form className="native-toolbar browser-toolbar" onSubmit={event => { event.preventDefault(); void navigate('navigate').catch(reportError); }}>
      <IconButton label={tr("Browser back")} disabled={state !== 'ready'} onClick={() => void navigate('back').catch(reportError)}><ArrowLeft size={14} /></IconButton>
      <IconButton label={tr("Browser forward")} disabled={state !== 'ready'} onClick={() => void navigate('forward').catch(reportError)}><ArrowRight size={14} /></IconButton>
      <IconButton label={tr("Reload browser")} disabled={state !== 'ready'} onClick={() => void navigate('reload').catch(reportError)}><RefreshCw size={14} /></IconButton>
      <Globe size={13}><title>{tr("Workspace owner network")}</title></Globe><input ref={addressBar} placeholder={tr("Enter address")} aria-label={tr("Browser address")} value={url} onBlur={() => { editing.current = false; }} onChange={event => { editing.current = true; setUrl(event.target.value); }} disabled={state !== 'ready'} />
      <span className="browser-control-slot">{controlled && browser && <IconButton label={tr("Take control")} title={tr("Agent control") + ' · ' + tr("Take control")} onClick={() => void window.luvus.browserTakeover(browser).catch(reportError)}><MousePointer2 size={14} /></IconButton>}</span>
    </form>
    <div className="browser-surface" ref={surface}>
      {state !== 'ready' && <Empty title={state === 'preparing' ? tr("Opening owner-network browser") : state === 'failed' ? tr("Browser could not start") : tr("Embedded browser")} action={state !== 'preparing' ? <AsyncButton className="button primary" run={open}>{tr("Open embedded browser")}</AsyncButton> : undefined}>
        {tr("The browser uses this workspace’s localhost and proxy settings. Closing the app revokes its connection.")}
      </Empty>}
    </div>
  </div>;
}
