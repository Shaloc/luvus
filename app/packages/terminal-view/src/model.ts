import type { Request, TerminalFrame } from '@luvus/app-protocol';

type Input = Extract<Request, { type: 'input' }>['input'];
/** Only adjacent keyboard phases can share a flight. Focus, resize and
 * coordinate-dependent commands must resolve before later input is sent. */
export function takeInputBatch(queue: Request[]): Request[] {
  let count = 1;
  if (queue[0]?.type === 'input' && queue[0].input.kind === 'key') {
    while (count < 8) {
      const next = queue[count];
      if (next?.type !== 'input' || next.input.kind !== 'key') break;
      count++;
    }
  }
  return queue.splice(0, count);
}
type Cell = TerminalFrame['cells'][number];
export type Point = [number, number];
export type SelectionMode = Extract<Input, { kind: 'selection' }>['mode'];
export interface Run { row: number; col: number; width: number; text: string; fg: number; bg: number; mods: number; symbol?: boolean }

/** Only the VT defines advances. Continuations contribute backgrounds, not text. */
export function styledRuns(cells: readonly Cell[]): Run[] {
  const runs: Run[] = [];
  let joinable = false;
  for (const cell of cells) {
    if (cell.width === 0) continue;
    const last = runs.at(-1);
    // Private-use icons often come from a different font with a different advance.
    const symbol = /[\uE000-\uF8FF\u{F0000}-\u{FFFFD}\u{100000}-\u{10FFFD}]/u.test(cell.text);
    const mono = cell.width === 1 && !symbol;
    if (joinable && mono && last && last.row === cell.row && last.col + last.width === cell.col && last.fg === cell.fg && last.bg === cell.bg && last.mods === cell.mods) {
      last.text += cell.text || ' '.repeat(cell.width);
      last.width += cell.width;
    } else runs.push({ row: cell.row, col: cell.col, width: cell.width, text: cell.text || ' '.repeat(cell.width), fg: cell.fg, bg: cell.bg, mods: cell.mods, ...(symbol && { symbol }) });
    // Fallback CJK/emoji fonts need not advance exactly two primary-font cells.
    joinable = mono;
  }
  return runs;
}

export function cellAt(frame: TerminalFrame, point: Point): Cell | undefined {
  const [row, col] = point;
  return frame.cells.find(cell => cell.row === row && cell.width > 0 && cell.col <= col && col < cell.col + cell.width);
}

export function cellPoint(x: number, y: number, width: number, height: number, cols: number, rows: number): Point {
  return [Math.max(0, Math.min(rows - 1, Math.floor(y / height))), Math.max(0, Math.min(cols - 1, Math.floor(x / width)))];
}

export function selectionRange(frame: TerminalFrame, anchor: Point, cursor: Point, mode: SelectionMode): [Point, Point] {
  let start: Point = [...anchor], end: Point = [...cursor];
  if (start[0] > end[0] || (start[0] === end[0] && start[1] > end[1])) [start, end] = [end, start];
  if (mode === 'line') return [[start[0], 0], [end[0], frame.cols - 1]];
  if (mode === 'word') {
    const whitespace = (point: Point) => /^\s*$/u.test(cellAt(frame, point)?.text ?? ' ');
    const left = whitespace(start), right = whitespace(end);
    while (start[1] > 0 && whitespace([start[0], start[1] - 1]) === left) start[1]--;
    while (end[1] < frame.cols - 1 && whitespace([end[0], end[1] + 1]) === right) end[1]++;
  }
  const first = cellAt(frame, start), last = cellAt(frame, end);
  if (first) start[1] = first.col;
  if (last) end[1] = last.col + last.width - 1;
  return [start, end];
}

/** Text layer only; copying always uses the core retained-selection extractor. */
export function accessibleText(frame: TerminalFrame): string {
  const rows: string[][] = Array.from({ length: frame.rows }, () => []);
  for (const cell of frame.cells) {
    if (cell.row >= frame.rows || cell.col >= frame.cols) continue;
    rows[cell.row]![cell.col] = cell.width === 0 ? '' : cell.text;
  }
  return rows.map((row, index) => {
    const text = Array.from({ length: frame.cols }, (_, col) => row[col] ?? ' ').join('');
    return frame.wraps[index] ? text : text.trimEnd();
  }).map((row, index) => row + (index < rows.length - 1 && !frame.wraps[index] ? '\n' : '')).join('');
}

export interface KeyLike {
  key: string; code: string; ctrlKey: boolean; altKey: boolean; shiftKey: boolean;
  metaKey: boolean; repeat: boolean; isComposing: boolean;
  getModifierState: (key: string) => boolean;
}

/** Distinguishes DOM text/IME from semantic keys without inventing ANSI. */
export function keyRoute(event: KeyLike): 'ignore' | 'text' | 'key' {
  if (event.metaKey || event.isComposing || ['Process', 'Dead', 'Shift', 'Control', 'Alt', 'Meta', 'CapsLock', 'Unidentified'].includes(event.key)) return 'ignore';
  if (event.getModifierState('AltGraph')) return 'text';
  if (Array.from(event.key).length === 1 && ((!event.ctrlKey && !event.altKey) || (event.altKey && !event.ctrlKey && event.key.codePointAt(0)! > 127))) return 'text';
  return 'key';
}

/** Normalize DOM button indices; 3 denotes no button for hover motion. */
export function terminalButton(button: number): number {
  return button === 0 ? 0 : button === 1 ? 1 : button === 2 ? 2 : 3;
}
