import { useEffect, useRef, useState } from 'react';
import { ArrowLeft, ArrowRight, Globe, RefreshCw } from 'lucide-react';
import { request, reportError } from './bridge';
import { record, targetKey, text, type Pane } from './model';
import { AsyncButton, Empty, IconButton } from './ui';

export function BrowserView({ pane }: { pane: Pane }) {
  const [browser, setBrowser] = useState<string | null>(null);
  const [state, setState] = useState('closed');
  const [controlled, setControlled] = useState(false);
  const [url, setUrl] = useState(text(pane.data.url, 'about:blank'));
  const surface = useRef<HTMLDivElement>(null);
  const identity = pane.target ? targetKey(pane.target) : pane.id;
  const currentIdentity = useRef(identity);
  currentIdentity.current = identity;
  useEffect(() => { setBrowser(null); setState('closed'); setControlled(false); setUrl(text(pane.data.url, 'about:blank')); }, [identity]);
  const apply = (value: unknown) => {
    const data = record(value);
    if (data.browser_id !== browser) return;
    setState(text(data.state, 'closed'));
    setControlled(data.controlled === true);
    if (typeof data.url === 'string') setUrl(data.url);
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
      const rect = surface.current?.getBoundingClientRect();
      const hidden = state !== 'ready' || !rect || document.querySelector('dialog[open]');
      const bounds = hidden ? null : {
        x: Math.max(0, rect.left), y: Math.max(0, rect.top),
        width: Math.max(0, Math.min(rect.right, window.innerWidth) - Math.max(0, rect.left)),
        height: Math.max(0, Math.min(rect.bottom, window.innerHeight) - Math.max(0, rect.top)),
      };
      void window.luvus.browserBounds(browser, bounds).catch(reportError);
    };
    const schedule = () => { if (!frame) frame = requestAnimationFrame(place); };
    const resize = new ResizeObserver(schedule);
    resize.observe(surface.current);
    const overlays = new MutationObserver(changes => {
      if (changes.some(change => change.target instanceof HTMLDialogElement || [...change.addedNodes, ...change.removedNodes].some(node => node instanceof HTMLDialogElement))) schedule();
    });
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
    if (!pane.target) throw new Error('Browser target is unavailable');
    setState('preparing');
    try {
      const result = record(await request({ type: 'browser_open', id: crypto.randomUUID(), target: pane.target }));
      if (currentIdentity.current !== identity) return;
      const id = text(result.browser_id);
      if (!id) throw new Error('Core did not return a browser identity');
      setBrowser(id);
    } catch (error) { if (currentIdentity.current === identity) { setState('failed'); throw error; } }
  };
  const navigate = async (action: 'navigate' | 'back' | 'forward' | 'reload') => {
    if (browser) await window.luvus.browserNavigate(browser, action, action === 'navigate' ? url : undefined);
  };
  return <div className="browser-view">
    <form className="native-toolbar browser-toolbar" onSubmit={event => { event.preventDefault(); void navigate('navigate').catch(reportError); }}>
      <IconButton label="Browser back" disabled={state !== 'ready'} onClick={() => void navigate('back').catch(reportError)}><ArrowLeft size={14} /></IconButton>
      <IconButton label="Browser forward" disabled={state !== 'ready'} onClick={() => void navigate('forward').catch(reportError)}><ArrowRight size={14} /></IconButton>
      <IconButton label="Reload browser" disabled={state !== 'ready'} onClick={() => void navigate('reload').catch(reportError)}><RefreshCw size={14} /></IconButton>
      <input aria-label="Browser address" value={url} onChange={event => setUrl(event.target.value)} disabled={state !== 'ready'} />
      {controlled && browser && <AsyncButton run={() => window.luvus.browserTakeover(browser)}>Take control</AsyncButton>}
    </form>
    <div className="browser-network-label"><Globe size={12} /><span>Workspace owner network</span>{controlled && <span className="badge">Agent control</span>}</div>
    <div className="browser-surface" ref={surface}>
      {state !== 'ready' && <Empty title={state === 'preparing' ? 'Opening owner-network browser' : state === 'failed' ? 'Browser could not start' : 'Embedded browser'} action={state !== 'preparing' ? <AsyncButton className="button primary" run={open}>Open embedded browser</AsyncButton> : undefined}>
        The browser uses this workspace’s localhost and proxy settings. Closing the app revokes its connection.
      </Empty>}
    </div>
  </div>;
}
