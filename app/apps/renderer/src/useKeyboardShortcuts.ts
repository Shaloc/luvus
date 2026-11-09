import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { commandKey, matchesChord, tabDigit } from './keyboard';
import { record, text, type Snapshot } from './model';
import { reportError } from './bridge';

export function useKeyboardShortcuts(snapshot: Snapshot | null, route: string, run: (command: string) => void | Promise<unknown>) {
  const [prefix, setPrefix] = useState(false);
  const mode = useRef('');
  const pending = useRef(''), consumed = useRef(new Set<string>()), running = useRef(false);
  const latest = useRef({snapshot,route,run}); latest.current = {snapshot,route,run};
  useLayoutEffect(() => { pending.current = ''; mode.current = ''; setPrefix(false);document.querySelector('[data-resizing]')?.removeAttribute('data-resizing'); }, [route]);
  useEffect(() => {
    let interaction=0,disposed=false;
    const cancel = () => { pending.current = ''; setPrefix(false); };
    const queue: {command:string;route:string}[]=[];
    const dispatch = (command: string) => {
      if(queue.length>=16) return;
      queue.push({command,route:latest.current.route});
      if(running.current) return;
      running.current = true;
      void (async()=>{
        try {
          while(queue.length) {
            const next=queue.shift()!;
            if(next.route!==latest.current.route) continue;
            const intent=interaction, generation=latest.current.snapshot?.generation;
            try {
              await latest.current.run(next.command);
              if (/^(tab:|focus_(left|right|up|down|back|forward)$|next_pane$|next_tab$|prev_tab$|new_tab$|split_|close_pane$|zoom_pane$|next_node$|prev_node$|jump_workspace_|next_attention$|next_done_agent$)/.test(next.command)) {
                await new Promise<void>(resolve=>requestAnimationFrame(()=>{
                  if(!disposed && intent===interaction && generation===latest.current.snapshot?.generation && !document.querySelector('dialog[open],[data-app-overlay]')) {
                    document.querySelector<HTMLTextAreaElement>('.pane-focused .terminal-host textarea')?.focus({preventScroll:true});
                  }
                  resolve();
                }));
              }
            } catch(error){reportError(error);}
          }
        } finally {running.current=false;}
      })();
    };
    const keydown = (event: KeyboardEvent) => {
      const {snapshot,route} = latest.current;
      const target = event.target instanceof HTMLElement ? event.target : null;
      if (target?.closest('[data-key-recorder]')) {cancel();return;}
      if (!snapshot || event.isComposing || event.key === 'Process' || document.querySelector('dialog[open], [data-app-overlay], [role="menu"], [role="listbox"]')) {cancel();mode.current='';return;}
      const digit = tabDigit(event);
      if (digit !== undefined) {
        event.preventDefault(); event.stopImmediatePropagation(); cancel();
        consumed.current.add(event.code); if (!event.repeat) dispatch('tab:'+digit); return;
      }
      if (target?.closest('input,textarea,[contenteditable="true"]') && !target.closest('.terminal-host')) {cancel();mode.current='';return;}
      if (event.metaKey || event.getModifierState('AltGraph') || ['Control','Shift','Alt','Meta','CapsLock'].includes(event.key)) return;
      if (target?.closest('[data-keyboard-copy]')) return;
      if (mode.current === route) {
        event.preventDefault();event.stopImmediatePropagation();consumed.current.add(event.code);
        const direction = ({arrowleft:'left',h:'left',arrowright:'right',l:'right',arrowup:'up',k:'up',arrowdown:'down',j:'down'} as Record<string,string>)[event.key.toLowerCase()];
        if (direction) dispatch('resize:'+direction+':'+(event.shiftKey?6:3));
        else if (['=','+','0'].includes(event.key)) dispatch('resize:equalize');
        else {mode.current='';dispatch('resize:end');}
        return;
      }
      const bindings = record(snapshot.raw.key_bindings), spec = text(bindings.prefix);
      const isPrefix = spec && matchesChord(event,spec);
      if (!isPrefix && (!pending.current || pending.current !== route)) { cancel(); return; }
      if (event.repeat) {event.preventDefault();event.stopImmediatePropagation();return;}
      if (pending.current === route && isPrefix) {
        // The second physical press reaches the existing terminal encoder, with
        // its release paired there. The first prefix never entered the PTY.
        cancel(); consumed.current.delete(event.code); return;
      }
      event.preventDefault(); event.stopImmediatePropagation(); consumed.current.add(event.code);
      if (!pending.current) {pending.current=route;setPrefix(true);return;}
      cancel();
      if (event.key === 'Escape') return;
      const suffix=commandKey(event);
      if (/^[1-9]$/.test(event.key) && !event.shiftKey) {dispatch('tab:'+event.key);return;}
      if (suffix === '?') {dispatch('help_keys');return;}
      if (['PageUp','PageDown','Home','End'].includes(event.key)) {dispatch('scroll:'+event.key);return;}
      const command = text(record(bindings.prefix_commands)[suffix]);
      if (command === 'resize_mode') mode.current=route;
      if (command) dispatch(command);
    };
    const keyup = (event: KeyboardEvent) => {
      if (consumed.current.delete(event.code)) {event.preventDefault();event.stopImmediatePropagation();}
    };
    const blur = () => {interaction++;queue.length=0;cancel();mode.current='';consumed.current.clear();document.querySelector('[data-resizing]')?.removeAttribute('data-resizing');};
    const pointer = () => {interaction++;queue.length=0;cancel();mode.current='';document.querySelector('[data-resizing]')?.removeAttribute('data-resizing');};
    window.addEventListener('keydown',keydown,true); window.addEventListener('keyup',keyup,true);
    window.addEventListener('pointerdown',pointer,true); window.addEventListener('blur',blur);
    const unsubscribe=window.luvus.onMessage(message=>{
      if (message.type!=='effect' || message.kind!=='desktop_tab_shortcut') return;
      const data=record(message.data), target=record(data.target), current=latest.current.snapshot;
      const workspace=current?.workspaces.find(w=>w.active), tab=workspace?.tabs.find(t=>t.active);
      if (target.generation!==current?.generation || target.workspace_id!==workspace?.id || target.tab_id!==tab?.id ||
          !tab?.panes.some(p=>p.id===target.pane_id && p.kind==='browser') || !Number.isInteger(data.position) || Number(data.position)<1 || Number(data.position)>9) return;
      cancel(); dispatch('tab:'+data.position);
    });
    return () => {
      disposed=true;queue.length=0;
      window.removeEventListener('keydown',keydown,true);window.removeEventListener('keyup',keyup,true);
      window.removeEventListener('pointerdown',pointer,true);window.removeEventListener('blur',blur);unsubscribe();
    };
  }, []);
  return prefix;
}
