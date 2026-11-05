import { useSyncExternalStore } from 'react';

export const defaultAppearance = {
  mode: 'system', language: 'system', uiFont: '-apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif',
  uiSize: 13, terminalFont: '"SFMono-Regular", Menlo, Monaco, Consolas, monospace', terminalSize: 13,
  wallpaperOpacity: 20, wallpaperFit: 'cover',
};
export type Appearance = typeof defaultAppearance;
const key = 'luvus.desktop.appearance.v1';
const listeners = new Set<() => void>();
let preferences = { ...defaultAppearance };
try {
  const saved = JSON.parse(localStorage.getItem(key) ?? '{}') as Partial<Appearance>;
  for (const field of ['uiFont', 'terminalFont'] as const) if (typeof saved[field] === 'string' && saved[field]!.length <= 256) preferences[field] = saved[field]!;
  if (['system', 'en', 'zh'].includes(saved.language ?? '')) preferences.language = saved.language!;
  if (['system', 'light', 'dark'].includes(saved.mode ?? '')) preferences.mode = saved.mode!;
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
  root.style.setProperty('--font-mono', preferences.terminalFont);
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
