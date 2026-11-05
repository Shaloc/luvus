import { useEffect, useRef, useState } from 'react';
import { ArrowUpRight, Search } from 'lucide-react';
import { message } from './model';
import { ErrorBox, Modal } from './ui';

export interface PaletteCommand { id: string; label: string; detail?: string; shortcut?: string; run: () => void | Promise<unknown> }
export function CommandPalette({ commands, onClose }: { commands: PaletteCommand[]; onClose: () => void }) {
  const [query, setQuery] = useState(''), [selected, setSelected] = useState(0), [error, setError] = useState<string | null>(null), [busy, setBusy] = useState(false);
  const list = useRef<HTMLDivElement>(null);
  const matches = commands.filter((command) => `${command.label} ${command.detail ?? ''}`.toLocaleLowerCase().includes(query.toLocaleLowerCase())).slice(0, 100);
  useEffect(() => { list.current?.querySelector('[aria-selected="true"]')?.scrollIntoView({ block: 'nearest' }); }, [selected]);
  const execute = async (command: PaletteCommand) => { if (busy) return; setBusy(true); try { await command.run(); onClose(); } catch (error) { setError(message(error)); setBusy(false); } };
  return <Modal title="Command palette" onClose={onClose}><div className="palette-search"><Search size={18} /><input autoFocus role="combobox" aria-label="Find command or workspace" aria-controls="command-results" aria-expanded="true" aria-activedescendant={matches[selected] ? `command-${selected}` : undefined} value={query} onChange={(event) => { setQuery(event.target.value); setSelected(0); }} placeholder="Search commands, workspaces, agents…" onKeyDown={(event) => {
    if (event.key === 'ArrowDown') { event.preventDefault(); setSelected((index) => Math.min(matches.length - 1, index + 1)); }
    if (event.key === 'ArrowUp') { event.preventDefault(); setSelected((index) => Math.max(0, index - 1)); }
    if (event.key === 'Enter' && matches[selected]) { event.preventDefault(); void execute(matches[selected]); }
  }} /></div>
    <div ref={list} className="palette-list" role="listbox" id="command-results">{matches.map((command, index) => <button id={`command-${index}`} key={command.id} role="option" aria-selected={selected === index} onPointerMove={() => setSelected(index)} onClick={() => { void execute(command); }} disabled={busy}><ArrowUpRight size={14} /><span><strong>{command.label}</strong>{command.detail && <small>{command.detail}</small>}</span>{command.shortcut && <kbd>{command.shortcut}</kbd>}</button>)}{!matches.length && <p className="muted">No matching desktop action. Low-frequency APIs are in Structured tools.</p>}</div>
    {error && <ErrorBox error={error} />}
  </Modal>;
}
