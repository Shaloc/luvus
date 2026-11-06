// Colors are packed by src/ipc/protocol.rs, not ANSI parsed in the renderer.
// Named ANSI colors belong to the displaying terminal. Keep them separate from
// explicit RGB and the fixed xterm color cube, which applications own.
const dark = ['#45475a', '#f38ba8', '#a6e3a1', '#f9e2af', '#89b4fa', '#cba6f7', '#94e2d5', '#bac2de', '#585b70', '#f38ba8', '#a6e3a1', '#f9e2af', '#89b4fa', '#cba6f7', '#94e2d5', '#cdd6f4'];
const light = ['#4c4f69', '#d20f39', '#40a02b', '#df8e1d', '#1e66f5', '#8839ef', '#179299', '#acb0be', '#5c5f77', '#d20f39', '#40a02b', '#df8e1d', '#1e66f5', '#8839ef', '#179299', '#bcc0cc'];
export function terminalAnsiPalette(isLight: boolean, roles: Record<string, unknown> = {}): readonly string[] {
  const colors = [...(isLight ? light : dark)];
  for (const [role, index] of [['red', 1], ['green', 2], ['yellow', 3], ['blue', 4], ['magenta', 5], ['cyan', 6]] as const) {
    const value = roles[role];
    if (typeof value === 'number' && value >>> 24 === 2) colors[index] = colors[index + 8] = unpackColor(value, colors[index]!);
  }
  return colors;
}
export function unpackColor(value: number, fallback: string, ansi: readonly string[] = dark): string {
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
