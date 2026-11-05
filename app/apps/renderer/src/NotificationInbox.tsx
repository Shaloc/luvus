import { useEffect, useState } from 'react';
import { Popover } from '@base-ui/react/popover';
import { Bell, X } from 'lucide-react';
import { markFallbackInboxRead, request, useCore, type InboxRow } from './bridge';
import { tr } from './i18n';
import { array, message, record, text, type Snapshot } from './model';
import { ErrorBox, Loading } from './ui';

export function NotificationInbox({ snapshot }: { snapshot: Snapshot }) {
  const { fallbackInbox, fallbackUnread } = useCore();
  const [open, setOpen] = useState(false);
  const [items, setItems] = useState<InboxRow[] | null>(null);
  const [error, setError] = useState('');
  const meta = record(snapshot.raw.notification_inbox);
  const revision = text(meta.revision), generation = snapshot.generation;
  const unread = revision ? Number(meta.unread) || 0 : fallbackUnread;
  useEffect(() => {
    if (!open) return;
    if (!revision) { markFallbackInboxRead(); return; }
    let live = true;
    setError('');
    void request({ type: 'notification_inbox', id: crypto.randomUUID(), generation, mark_read: unread > 0 }).then(value => {
      if (live) setItems(array(record(value).items).map(value => {
        const row = record(value);
        return { id: text(row.id), text: text(row.text), level: text(row.level), time: Number(row.time), count: Number(row.count) };
      }));
    }).catch(reason => { if (live) setError(message(reason)); });
    return () => { live = false; };
  }, [open, generation, revision, unread]);
  const rows = revision ? items : fallbackInbox;
  return <Popover.Root open={open} onOpenChange={next => { setOpen(next); if (!next) setItems(null); }}>
    <Popover.Trigger className="inbox-trigger" aria-label={tr("Notification inbox")} title={tr("Notification inbox")}>
      <Bell size={13} />{unread > 0 && <b>{unread}</b>}
    </Popover.Trigger>
    <Popover.Portal><Popover.Positioner side="top" align="end" sideOffset={5} className="choice-positioner">
      <Popover.Popup className="session-popup inbox-popup" data-app-overlay>
        <div className="session-popup-heading"><Popover.Title>{tr("Notification inbox")}</Popover.Title>
          <Popover.Close className="icon-button" aria-label={tr("Close")}><X size={13}/></Popover.Close></div>
        {error ? <ErrorBox error={error} /> : !rows ? <Loading /> : rows.length === 0 ? <p className="sidebar-empty">{tr("No notifications")}</p> :
          <ol className="inbox-list">{rows.map(row => <li key={row.id} data-level={row.level}><span>{row.text}</span>
            <small>{new Date(row.time * 1000).toLocaleTimeString(document.documentElement.lang, { hour: '2-digit', minute: '2-digit' })}{row.count > 1 && ' ×' + row.count}</small></li>)}</ol>}
        {!revision && <small className="inbox-source">{tr("Showing notifications received by this window.")}</small>}
      </Popover.Popup>
    </Popover.Positioner></Popover.Portal>
  </Popover.Root>;
}
