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
