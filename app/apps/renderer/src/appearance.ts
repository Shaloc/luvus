import { useMemo, useSyncExternalStore } from 'react';
import { terminalAnsiPalette, unpackColor } from '@luvus/terminal-view';
import { record } from './model';

const lightSystem = window.matchMedia('(prefers-color-scheme: light)');
const subscribeSystem = (listener: () => void) => {
  lightSystem.addEventListener('change', listener);
  return () => lightSystem.removeEventListener('change', listener);
};
function luminance(color: string) {
  const channels = color.startsWith('#')
    ? [1, 3, 5].map(offset => parseInt(color.slice(offset, offset + 2), 16))
    : (color.match(/\d+/g) ?? []).map(Number);
  return channels.reduce((sum, value, i) => {
    const s = value / 255;
    return sum + (s <= .04045 ? s / 12.92 : ((s + .055) / 1.055) ** 2.4) * [0.2126, 0.7152, 0.0722][i]!;
  }, 0);
}
export function useTerminalPalette(mode: string, projected: unknown) {
  const systemIsLight = useSyncExternalStore(subscribeSystem, () => lightSystem.matches);
  const light = mode === 'light' || (mode === 'system' && systemIsLight);
  const roles = record(record(projected).ansi);
  const roleKey = JSON.stringify(roles);
  const ansi = useMemo(() => terminalAnsiPalette(light, JSON.parse(roleKey)), [light, roleKey]);
  const fallbackAnsi = useMemo(() => terminalAnsiPalette(light), [light]);
  const fallback = light ? { background: '#fafafa', foreground: '#242529', ansi: fallbackAnsi } : { background: '#191a1d', foreground: '#e2e3e7', ansi: fallbackAnsi };
  const palette = record(projected);
  const background = unpackColor(Number(palette.background), fallback.background);
  const foreground = unpackColor(Number(palette.foreground), fallback.foreground);
  const bg = luminance(background), fg = luminance(foreground);
  // A shared core can use a different theme from this display. Only adopt its
  // default colors when they fit the device mode and keep plain text legible.
  if ((light ? bg >= .5 && fg < bg : bg <= .18 && fg > bg) &&
      (Math.max(bg, fg) + .05) / (Math.min(bg, fg) + .05) >= 4.5) return { background, foreground, ansi };
  return fallback;
}

export const defaultAppearance = {
  mode: 'system', language: 'system', uiFont: '-apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif',
  uiSize: 13, terminalFont: '"SFMono-Regular", Menlo, Monaco, Consolas, monospace', terminalSize: 13,
  wallpaperOpacity: 20, wallpaperFit: 'cover',
  confirmTabClose: true,
};
export function terminalFontStack(family: string): string {
  const preferred = family.replace(/(?:^|,\s*)monospace\s*$/i, '').trim();
  return (preferred ? preferred + ', ' : '') + '"Luvus Nerd Symbols", monospace';
}
export type Appearance = typeof defaultAppearance;
const key = 'luvus.desktop.appearance.v1';
const listeners = new Set<() => void>();
let preferences = { ...defaultAppearance };
try {
  const saved = JSON.parse(localStorage.getItem(key) ?? '{}') as Partial<Appearance>;
  for (const field of ['uiFont', 'terminalFont'] as const) if (typeof saved[field] === 'string' && saved[field]!.length <= 256) preferences[field] = saved[field]!;
  if (['system', 'en', 'zh'].includes(saved.language ?? '')) preferences.language = saved.language!;
  if (['system', 'light', 'dark'].includes(saved.mode ?? '')) preferences.mode = saved.mode!;
  if (typeof saved.confirmTabClose === 'boolean') preferences.confirmTabClose = saved.confirmTabClose;
  if (['cover', 'contain'].includes(saved.wallpaperFit ?? '')) preferences.wallpaperFit = saved.wallpaperFit!;
  for (const [field, min, max] of [['uiSize', 11, 18], ['terminalSize', 9, 32], ['wallpaperOpacity', 0, 100]] as const) {
    const value = saved[field]; if (typeof value === 'number' && Number.isFinite(value)) preferences[field] = Math.max(min, Math.min(max, value));
  }
} catch { /* Invalid device preferences fall back without touching core config. */ }
let wallpaper = '';
let imageRevision = 0;
function emit() { for (const listener of listeners) listener(); }
const subscribe = (listener: () => void) => { listeners.add(listener); return () => { listeners.delete(listener); }; };
export function useAppearance() { return useSyncExternalStore(subscribe, () => preferences); }
export function useWallpaper() { return useSyncExternalStore(subscribe, () => wallpaper); }
export function setAppearance(patch: Partial<Appearance>) {
  const next = { ...preferences, ...patch };
  localStorage.setItem(key, JSON.stringify(next));
  preferences = next; applyAppearance(); emit();
}
export function applyAppearance() {
  const root = document.documentElement;
  root.lang = preferences.language === 'zh' || (preferences.language === 'system' && navigator.language.toLowerCase().startsWith('zh')) ? 'zh-CN' : 'en';
  root.dataset.mode = preferences.mode;
  root.style.setProperty('--font-sans', preferences.uiFont);
  root.style.setProperty('--font-mono', terminalFontStack(preferences.terminalFont));
  root.style.setProperty('--ui-size', `${preferences.uiSize}px`);
  root.style.setProperty('--wallpaper-opacity', String(preferences.wallpaperOpacity / 100));
  root.style.setProperty('--wallpaper-fit', preferences.wallpaperFit);
  root.style.setProperty('--wallpaper', wallpaper ? `url("${wallpaper}")` : 'none');
  root.dataset.wallpaper = wallpaper ? 'true' : 'false';
}
async function storeImage(blob?: Blob | null): Promise<Blob | null> {
  const db = await new Promise<IDBDatabase>((resolve, reject) => {
    const request = indexedDB.open('luvus-device-images', 1);
    request.onupgradeneeded = () => request.result.createObjectStore('wallpaper');
    request.onerror = () => reject(request.error); request.onsuccess = () => resolve(request.result);
  });
  try {
    return await new Promise((resolve, reject) => {
      const transaction = db.transaction('wallpaper', blob === undefined ? 'readonly' : 'readwrite');
      const object = transaction.objectStore('wallpaper');
      const request = blob === undefined ? object.get('current') : blob === null ? object.delete('current') : object.put(blob, 'current');
      transaction.oncomplete = () => resolve(blob === undefined ? (request.result instanceof Blob ? request.result : null) : blob);
      transaction.onerror = () => reject(transaction.error); transaction.onabort = () => reject(transaction.error);
    });
  } finally { db.close(); }
}
function replaceImage(blob: Blob | null) {
  if (wallpaper) URL.revokeObjectURL(wallpaper);
  wallpaper = blob ? URL.createObjectURL(blob) : '';
  applyAppearance(); emit();
}
export async function chooseWallpaper(file: File | null) {
  const revision = ++imageRevision;
  if (file) {
    if (!['image/png', 'image/jpeg', 'image/webp'].includes(file.type) || file.size > 20 * 1024 * 1024) throw Error('Choose a PNG, JPEG or WebP image up to 20 MB.');
    const bitmap = await createImageBitmap(file);
    const pixels = bitmap.width * bitmap.height; bitmap.close();
    if (pixels > 32_000_000) throw Error('Choose an image smaller than 32 megapixels.');
  }
  if (revision !== imageRevision) return;
  await storeImage(file);
  if (revision === imageRevision) replaceImage(file);
}
window.addEventListener('languagechange', () => { preferences = {...preferences}; applyAppearance(); emit(); });
applyAppearance();
void storeImage().then(blob => { if (imageRevision === 0) replaceImage(blob); }).catch(() => {});
