import { useLayoutEffect, useRef, type ReactNode } from 'react';
import { createPortal } from 'react-dom';

export interface MenuEntry { label: string; icon?: ReactNode; disabled?: boolean; danger?: boolean; group?: boolean; run: () => void }
export interface MenuPoint { x: number; y: number }

export function PointerMenu({ point, entries, close }: { point: MenuPoint; entries: MenuEntry[]; close: () => void }) {
  const ref = useRef<HTMLDivElement>(null), latest = useRef(close);
  latest.current = close;
  useLayoutEffect(() => {
    const menu = ref.current!;
    const previous = document.activeElement;
    menu.style.left = Math.max(4, Math.min(point.x, innerWidth - menu.offsetWidth - 4)) + 'px';
    menu.style.top = Math.max(4, Math.min(point.y, innerHeight - menu.offsetHeight - 4)) + 'px';
    const buttons = () => [...menu.querySelectorAll<HTMLButtonElement>('button:not(:disabled)')];
    buttons()[0]?.focus({ preventScroll: true });
    const outside = (e: PointerEvent) => { if (!menu.contains(e.target as Node)) latest.current(); };
    const dismiss = () => latest.current();
    document.addEventListener('pointerdown', outside, true);
    window.addEventListener('resize', dismiss); window.addEventListener('blur', dismiss);
    return () => {
      document.removeEventListener('pointerdown', outside, true);
      window.removeEventListener('resize', dismiss); window.removeEventListener('blur', dismiss);
      if ((menu.contains(document.activeElement) || document.activeElement === document.body) && previous instanceof HTMLElement && previous.isConnected) previous.focus({ preventScroll: true });
    };
  }, [point]);
  return createPortal(<div ref={ref} role="menu" className="pointer-menu" data-app-overlay onContextMenu={e => e.preventDefault()} onKeyDown={e => {
    if (e.key === 'Escape' || e.key === 'Tab') { e.preventDefault(); close(); return; }
    const buttons = [...e.currentTarget.querySelectorAll<HTMLButtonElement>('button:not(:disabled)')];
    const at = buttons.indexOf(document.activeElement as HTMLButtonElement);
    const next = e.key === 'Home' ? 0 : e.key === 'End' ? buttons.length - 1 : e.key === 'ArrowDown' ? (at + 1) % buttons.length : e.key === 'ArrowUp' ? (at + buttons.length - 1) % buttons.length : -1;
    if (next >= 0) { e.preventDefault(); buttons[next]?.focus(); }
  }}>{entries.map(item => <button role="menuitem" key={item.label} disabled={item.disabled} data-group={item.group || undefined} className={item.danger ? 'destructive-text' : undefined} onClick={() => { close(); item.run(); }}>{item.icon}{item.label}</button>)}</div>, document.body);
}
