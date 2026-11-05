// Colors are packed by src/ipc/protocol.rs, not ANSI parsed in the renderer.
const ansi = ['#000000', '#800000', '#008000', '#808000', '#000080', '#800080', '#008080', '#c0c0c0', '#808080', '#ff0000', '#00ff00', '#ffff00', '#0000ff', '#ff00ff', '#00ffff', '#ffffff'];
export function unpackColor(value: number, fallback: string): string {
  const kind = value >>> 24;
  if (kind === 2) return `#${(value & 0xffffff).toString(16).padStart(6, '0')}`;
  if (kind !== 1) return fallback;
  const index = value & 255;
  if (index < 16) return ansi[index] ?? fallback;
  if (index >= 232) { const gray = 8 + (index - 232) * 10; return `rgb(${gray} ${gray} ${gray})`; }
  const cube = index - 16;
  const level = (n: number) => n === 0 ? 0 : 55 + n * 40;
  return `rgb(${level(Math.floor(cube / 36))} ${level(Math.floor(cube / 6) % 6)} ${level(cube % 6)})`;
}
export function safeLink(value: string): string | null {
  try {
    const url = new URL(value);
    return (url.protocol === 'https:' || url.protocol === 'http:') && !url.username && !url.password ? url.href : null;
  } catch { return null; }
}
