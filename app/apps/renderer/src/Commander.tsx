import { tr } from './i18n';
import { useState } from 'react';
import { Send, X } from 'lucide-react';
import { action } from './bridge';
import { validateContext, type OpenDialog } from './Actions';
import { message, targetKey, type Snapshot } from './model';
import { IconButton, StatusDot } from './ui';

export function Commander({ snapshot, openDialog, onClose, visible }: { snapshot: Snapshot; openDialog: OpenDialog; onClose: () => void; visible: boolean }) {
  const agents = snapshot.workspaces.flatMap((workspace) => workspace.tabs.flatMap((tab) => tab.panes.filter((pane) => pane.isAgent && pane.target?.terminal_id).map((pane) => ({ pane, tab, workspace, key: targetKey(pane.target!) }))));
  const [draft, setDraft] = useState(''), [selected, setSelected] = useState<string[]>([]);
  const [receipts, setReceipts] = useState<{ name: string; message: string; ok: boolean }[]>([]);
  const targets = agents.filter((agent) => selected.includes(agent.key));
  return <section className="commander" hidden={!visible} aria-label={tr("Commander prompt delivery")}><header><strong>{tr("Commander")}</strong><span className="muted">{targets.length} {tr("targets")}</span><IconButton label={tr("Close Commander")} onClick={onClose}><X size={15} /></IconButton></header>
    <div className="commander-targets">{agents.map(({ pane, workspace, key }) => <label className="target-chip" key={key}><input type="checkbox" checked={selected.includes(key)} onChange={(event) => setSelected((old) => event.target.checked ? [...old, key] : old.filter((value) => value !== key))} /><StatusDot state={pane.agentStatus} /><span>{pane.title}</span><small>{workspace.name}</small></label>)}{!agents.length && <span className="muted">{tr("No available agent terminals.")}</span>}</div>
    <div className="commander-composer"><textarea aria-label={tr("Commander prompt")} rows={2} placeholder={tr("Write a prompt for selected agents…")} value={draft} onChange={(event) => setDraft(event.target.value)} /><button className="button primary" disabled={!targets.length || !draft.trim()} onClick={() => {
      const prompt = draft;
      openDialog({ title: tr("Review prompt delivery"), submit: `Send to ${targets.length} agent${targets.length === 1 ? '' : 's'}`, description: `Targets: ${targets.map(({ pane, workspace }) => `${pane.title} (${workspace.name})`).join(', ')}\n\n${prompt}\n\nEach target is checked independently for strict composer readiness. Delivery is not task completion.`, run: async () => {
        const results: { name: string; message: string; ok: boolean }[] = [];
        for (const target of targets) {
          try {
            validateContext(snapshot, target.workspace, target.tab, target.pane);
            await action('agent.prompt', { target: target.pane.id, terminal_id: target.pane.target?.terminal_id, text: prompt, strict: true }, snapshot.generation);
            results.push({ name: target.pane.title, message: 'Queued by core', ok: true });
          } catch (error) { results.push({ name: target.pane.title, message: message(error), ok: false }); }
        }
        setReceipts(results);
        // Keep failed targets selected; never repeat already delivered prompts implicitly.
        setSelected(targets.filter((_, index) => !results[index]?.ok).map((target) => target.key));
        if (results.every((result) => result.ok)) setDraft('');
      } });
    }}><Send size={14} />{tr("Review & send")}</button></div>
    {receipts.length > 0 && <div className="receipts" aria-live="polite">{receipts.map((receipt, index) => <p key={index} className={receipt.ok ? 'muted' : 'destructive-text'}><strong>{receipt.name}:</strong> {receipt.message}</p>)}</div>}
    <p className="commander-note">{tr("Plain prompt delivery only. Core Commander slash actions, images and prepare/deliver receipts are not exposed in this protocol.")}</p>
  </section>;
}
