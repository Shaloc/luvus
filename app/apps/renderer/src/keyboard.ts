export interface KeyStroke {
  key: string; ctrlKey: boolean; altKey: boolean; shiftKey: boolean; metaKey: boolean;
}

// Prefix specifications come from the core's effective key map, including overrides.
export function matchesChord(event: KeyStroke, chord: string): boolean {
  const parts = chord.toLowerCase().split('+');
  const key = parts.pop();
  const name = key === 'space' ? ' ' : key === 'plus' ? '+' : key;
  return !event.metaKey && event.ctrlKey === parts.includes('ctrl') && event.altKey === parts.includes('alt') &&
    event.shiftKey === parts.includes('shift') && event.key.toLowerCase() === name;
}

export function commandKey(event: KeyStroke): string {
  const special: Record<string, string> = {ArrowLeft:'←',ArrowRight:'→',ArrowUp:'↑',ArrowDown:'↓',Tab:event.shiftKey?'⇧⇥':'⇥',Enter:'Enter'};
  if (special[event.key]) return special[event.key]!;
  const plain = '`1234567890-=[]\\;\',./', shifted = '~!@#$%^&*()_+{}|:"<>?';
  const index = plain.indexOf(event.key);
  return event.shiftKey && event.key.length === 1 && index >= 0 ? shifted[index]! : event.key;
}

export function tabDigit(event: KeyStroke): number | undefined {
  if (event.metaKey && !event.ctrlKey && !event.altKey && !event.shiftKey && /^[1-9]$/.test(event.key)) return Number(event.key);
}

// Match the core's PrefixSpec, DirectKeySpec and key_string accepted domains.
export function recordedBinding(event: KeyStroke, mode: 'prefix'|'key'|'direct'): string | undefined {
  const character = [...event.key].length === 1;
  if (event.metaKey) return;
  if (mode === 'key') {
    if (event.ctrlKey || event.altKey) return;
    if (character || ['ArrowLeft','ArrowRight','ArrowUp','ArrowDown','Tab','Enter'].includes(event.key)) return commandKey(event);
    return;
  }
  const fn = /^F([1-9]|1[0-2])$/.test(event.key);
  const ascii = character && event.key.charCodeAt(0) >= 32 && event.key.charCodeAt(0) <= 126;
  if (mode === 'prefix') {
    if (!fn && (!ascii || !(event.ctrlKey || event.altKey) || event.shiftKey)) return;
  } else {
    if (!(event.ctrlKey || event.altKey || event.shiftKey)) return;
    if (ascii ? !(event.ctrlKey || event.altKey) : !fn && !['ArrowLeft','ArrowRight','ArrowUp','ArrowDown','Home','End','PageUp','PageDown','Tab','Enter','Escape','Delete','Insert'].includes(event.key)) return;
  }
  const name = event.key === ' ' ? 'space' : event.key === '+' ? 'plus' : event.key.toLowerCase().replace('arrow','');
  return [event.ctrlKey?'ctrl':'',event.altKey?'alt':'',event.shiftKey?'shift':'',name].filter(Boolean).join('+');
}

export function bindingPatch(overrides: Record<string,unknown>, id: string, key: string, direct: boolean): Record<string,string> {
  const canonical = (value: string) => {
    if (!direct) return value;
    const aliases: Record<string,string> = {control:'ctrl',option:'alt',opt:'alt',meta:'alt',spc:'space',return:'enter',esc:'escape',del:'delete',ins:'insert','page-up':'pageup','page-down':'pagedown'};
    const parts=value.toLowerCase().split('+').map(part=>part.trim()).filter(Boolean).map(part=>aliases[part]??part);
    const modifiers=['ctrl','alt','shift'];
    return [...modifiers.filter(part=>parts.includes(part)),...parts.filter(part=>!modifiers.includes(part))].join('+');
  };
  const patch: Record<string,string> = {};
  for (const [other,value] of Object.entries(overrides)) {
    if (other !== id && typeof value === 'string' && value && canonical(value) === canonical(key)) patch[other] = '';
  }
  patch[id] = key;
  return patch;
}
