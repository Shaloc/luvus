import { tr } from './i18n';
import { useEffect, useId, useRef, useState, type ButtonHTMLAttributes, type ReactNode } from 'react';
import { AlertCircle, Check, ChevronDown, LoaderCircle, X } from 'lucide-react';
import { Select } from '@base-ui/react/select';
import { label, message, record } from './model';

export function IconButton({ label: title, children, ...props }: ButtonHTMLAttributes<HTMLButtonElement> & { label: string; children: ReactNode }) {
  return <button type="button" className="icon-button" title={title} aria-label={title} {...props}>{children}</button>;
}
export function Empty({ title, children, action }: { title: string; children?: ReactNode; action?: ReactNode }) {
  return <div className="empty-state"><h3>{title}</h3>{children && <p>{children}</p>}{action}</div>;
}
export function Loading() { return <div className="loading" role="status"><LoaderCircle size={16} className="spin" /> {tr("Loading from core…")}</div>; }
export function ErrorBox({ error }: { error: string }) { return <div className="error-box" role="alert"><AlertCircle size={16} /><span>{error}</span></div>; }
export function Notice({ children }: { children: ReactNode }) { return <div className="notice">{children}</div>; }
export function Modal({ title, children, onClose, wide = false }: { title: string; children: ReactNode; onClose: () => void; wide?: boolean }) {
  const ref = useRef<HTMLDialogElement>(null), heading = useId();
  useEffect(() => {
    const previous = document.activeElement;
    const focusVisible = previous?.matches(':focus-visible') ?? false;
    ref.current?.showModal();
    ref.current?.querySelector<HTMLElement>('input:not([type="checkbox"]), textarea, select')?.focus();
    return () => { ref.current?.close(); if (previous instanceof HTMLElement && previous.isConnected) previous.focus({ preventScroll: true, focusVisible } as FocusOptions); };
  }, []);
  return <dialog ref={ref} className={`modal ${wide ? 'modal-wide' : ''}`} aria-labelledby={heading} onCancel={(event) => { event.preventDefault(); onClose(); }} onClick={(event) => { if (event.target === event.currentTarget) onClose(); }}>
    <div className="modal-inner"><header><h2 id={heading}>{title}</h2><IconButton label={tr("Close dialog")} onClick={onClose}><X size={16} /></IconButton></header>{children}</div>
  </dialog>;
}
export function AsyncButton({ run, children, className = 'button', ...props }: Omit<ButtonHTMLAttributes<HTMLButtonElement>, 'onClick'> & { run: () => Promise<unknown> }) {
  const [busy, setBusy] = useState(false), [error, setError] = useState<string | null>(null);
  return <><button type="button" className={className} {...props} disabled={busy || props.disabled} onClick={() => { setBusy(true); setError(null); void run().catch((error: unknown) => setError(message(error))).finally(() => setBusy(false)); }}>{busy && <LoaderCircle size={14} className="spin" />}{children}</button>{error && <ErrorBox error={error} />}</>;
}
export function Detail({ value, depth = 0 }: { value: unknown; depth?: number }) {
  if (value === null || value === undefined) return <span className="muted">{tr("Not available")}</span>;
  if (typeof value === 'boolean') return <span>{value ? tr("Yes") : tr("No")}</span>;
  if (typeof value !== 'object') return <span className="selectable">{String(value)}</span>;
  if (depth > 5) return <span className="muted">{tr("Nested data omitted")}</span>;
  if (Array.isArray(value)) return <div className="detail-list">{value.slice(0, 256).map((item, index) => <div key={index}><Detail value={item} depth={depth + 1} /></div>)}{value.length > 256 && <Notice>{tr("Showing 256 of")} {value.length} {tr("items.")}</Notice>}{!value.length && <span className="muted">{tr("None")}</span>}</div>;
  return <dl className="details">{Object.entries(record(value)).filter(([key]) => key !== 'type').map(([key, item]) => <div key={key}><dt>{label(key)}</dt><dd><Detail value={item} depth={depth + 1} /></dd></div>)}</dl>;
}
export function StatusDot({ state }: { state: string }) { return <span className={`status-dot status-${['working', 'busy', 'running', 'waiting', 'idle', 'done', 'error'].includes(state) ? state : 'unknown'}`} title={state || tr("Unknown status")} aria-label={state || tr("Unknown status")} />; }

export function Choice({ label, value, options, onChange, disabled = false }: { label: string; value: string; options: { value: string; label: string }[]; onChange: (value: string) => void; disabled?: boolean }) {
  const trigger = useRef<HTMLButtonElement>(null);
  const [container, setContainer] = useState<HTMLElement | null>(null);
  // Native dialogs are in the top layer; keep their menus in the same layer.
  useEffect(() => { setContainer(trigger.current?.closest('dialog') ?? document.body); }, []);
  return <Select.Root modal={false} value={value} items={options} disabled={disabled} onValueChange={next => { if (next !== null) onChange(next); }}>
    <Select.Trigger ref={trigger} className="choice-trigger" aria-label={label}><Select.Value /><Select.Icon><ChevronDown size={14} /></Select.Icon></Select.Trigger>
    <Select.Portal container={container}><Select.Positioner positionMethod="fixed" className="choice-positioner" sideOffset={5} alignItemWithTrigger={false}>
      <Select.Popup className="choice-popup" data-app-overlay><Select.List>{options.map(option => <Select.Item className="choice-option" key={option.value} value={option.value} data-value={option.value}>
        <Select.ItemIndicator className="choice-check"><Check size={13} /></Select.ItemIndicator><Select.ItemText>{option.label}</Select.ItemText>
      </Select.Item>)}</Select.List></Select.Popup>
    </Select.Positioner></Select.Portal>
  </Select.Root>;
}
