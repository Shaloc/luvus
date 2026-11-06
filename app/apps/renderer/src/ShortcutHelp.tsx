import { tr } from './i18n';
import { record, text, type Snapshot } from './model';
import { Modal } from './ui';

export function ShortcutHelp({snapshot,onClose}:{snapshot:Snapshot;onClose:()=>void}) {
  const bindings=record(snapshot.raw.key_bindings), keys=record(bindings.prefix_commands);
  return <Modal title={tr('Keyboard shortcuts')} onClose={onClose}>
    <div className="shortcut-list"><div><span>{tr('Switch tab')}</span><kbd>⌘ 1–9</kbd></div>
      <div><span>{tr('Prefix')}</span><kbd>{text(bindings.prefix)}</kbd></div>
      <div><span>{tr('Switch tab')}</span><kbd>{text(bindings.prefix)} → 1–9</kbd></div>
      {snapshot.commands.map(command=>{
        const assigned=Object.entries(keys).filter(([,value])=>value===command.id).map(([key])=>key);
        return assigned.length ? <div key={text(command.id)}><span>{text(command.label)}</span><kbd>{assigned.join(' / ')}</kbd></div> : null;
      })}
    </div>
  </Modal>;
}
