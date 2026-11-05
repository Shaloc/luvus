import { tr } from './i18n';
import { useEffect, useState } from 'react';
import { Popover } from '@base-ui/react/popover';
import { Check, ChevronDown, Plus, RefreshCw, Search, Server, Settings2 } from 'lucide-react';
import { useCore } from './bridge';
import { array, message, record, text, type RecordValue, type Snapshot } from './model';
import { type OpenDialog } from './Actions';
import { ErrorBox, IconButton, Loading } from './ui';

export function SessionSelector({ snapshot, openDialog, onManage }: { snapshot: Snapshot; openDialog: OpenDialog; onManage: () => void }) {
  const { remoteHost } = useCore();
  const [open, setOpen] = useState(false), [filter, setFilter] = useState('');
  const [rows, setRows] = useState<RecordValue[] | null>(null), [error, setError] = useState(''), [busy, setBusy] = useState(false);
  const [revision, setRevision] = useState(0);
  useEffect(() => {
    if (!open || remoteHost) return;
    let live = true;
    setRows(null); setError('');
    void window.luvus.sessions('list', '', snapshot.generation).then(value => {
      if (live) setRows(array(record(value).sessions).map(record));
    }).catch(reason => { if (live) setError(message(reason)); });
    return () => { live = false; };
  }, [open, snapshot.generation, remoteHost, revision]);
  const select = async (name: string) => {
    if (name === snapshot.session) { setOpen(false); return; }
    setBusy(true); setError('');
    try { await window.luvus.sessions('switch', name, snapshot.generation); setOpen(false); }
    catch (reason) { setError(message(reason)); }
    finally { setBusy(false); }
  };
  return <Popover.Root open={open} onOpenChange={value => { if (!busy) { setOpen(value); setFilter(''); } }}>
    <Popover.Trigger className="session-selector" aria-label={tr("Switch session")} title={snapshot.session}>
      <Server size={15} /><span className="session-name">{snapshot.session}{remoteHost && <small> / {remoteHost}</small>}</span><ChevronDown size={13} />
    </Popover.Trigger>
    <Popover.Portal><Popover.Positioner className="choice-positioner" sideOffset={6} align="start">
      <Popover.Popup className="session-popup" data-app-overlay>
        <div className="session-popup-heading"><Popover.Title>{tr("Sessions")}</Popover.Title><IconButton label={tr("Refresh sessions")} disabled={busy || Boolean(remoteHost)} onClick={() => setRevision(value => value + 1)}><RefreshCw size={14} /></IconButton></div>
        {remoteHost ? <p className="sidebar-empty">{remoteHost} / {snapshot.session}</p> : <>
          <div className="filter-input"><Search size={14} /><input aria-label={tr("Find session")} autoFocus placeholder={tr("Find session")} value={filter} onChange={event => setFilter(event.target.value)} /></div>
          {error && <ErrorBox error={error} />}
          {!rows && !error ? <Loading /> : <div className="session-list" aria-label={tr("Sessions")}>{rows?.filter(row => text(row.name).toLocaleLowerCase().includes(filter.toLocaleLowerCase())).map(row => <button className="session-option" key={text(row.name)} disabled={busy} aria-current={row.name === snapshot.session ? 'true' : undefined} onClick={() => void select(text(row.name))}>
            <Server size={14} /><span><strong>{text(row.name)}</strong><small>{row.running ? tr("Running") : tr("Stopped")}</small></span>{row.name === snapshot.session && <Check size={14} />}
          </button>)}{rows && !rows.some(row => text(row.name).toLowerCase().includes(filter.toLowerCase())) && <p className="sidebar-empty">{tr("No matching sessions")}</p>}</div>}
          <button className="session-option" disabled={busy} onClick={() => { setOpen(false); openDialog({ title: tr("Create local session"), submit: tr("Create and switch"), fields: [{ key: 'name', label: tr("Session name"), required: true, pattern: '[a-zA-Z0-9_.][a-zA-Z0-9_.\\-]{0,63}' }], run: value => window.luvus.sessions('create', text(value.name), snapshot.generation) }); }}><Plus size={14} />{tr("New session")}</button>
        </>}
        <button className="session-option session-manage" disabled={busy} onClick={() => { setOpen(false); onManage(); }}><Settings2 size={14} />{tr("Manage connections")}</button>
      </Popover.Popup>
    </Popover.Positioner></Popover.Portal>
  </Popover.Root>;
}
