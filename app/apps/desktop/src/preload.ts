import { contextBridge, ipcRenderer } from 'electron';
import type { Request, Response } from '@luvus/app-protocol';

contextBridge.exposeInMainWorld('luvus', Object.freeze({
  request: (request: Request): Promise<unknown> => ipcRenderer.invoke('luvus:request', request),
  reconnect: (): Promise<void> => ipcRenderer.invoke('luvus:reconnect'),
  bootstrap: (): Promise<Response[]> => ipcRenderer.invoke('luvus:bootstrap'),
  terminalPaste: (target: unknown) => ipcRenderer.invoke('luvus:terminal-paste',target),
  sessions: (op:string, name:unknown, generation:string): Promise<unknown> => ipcRenderer.invoke('luvus:sessions',op,name,generation),
  gateway:(op:string,kind:string,control:boolean,generation:string):Promise<unknown>=>ipcRenderer.invoke('luvus:gateway',op,kind,control,generation),
  addons:(op:string,agent:string,generation:string):Promise<unknown>=>ipcRenderer.invoke('luvus:addons',op,agent,generation),
  chooseDirectory: (): Promise<string | null> => ipcRenderer.invoke('luvus:choose-directory'),
  browserBounds: (id: string, bounds: { x: number; y: number; width: number; height: number } | null): Promise<void> => ipcRenderer.invoke('luvus:browser-bounds', id, bounds),
  browserStatus: (id: string): Promise<unknown> => ipcRenderer.invoke('luvus:browser-status', id),
  browserNavigate: (id: string, action: 'navigate' | 'back' | 'forward' | 'reload', bounds: Electron.Rectangle, url?: string): Promise<void> => ipcRenderer.invoke('luvus:browser-navigate', id, action, bounds, url),
  browserTakeover: (id: string): Promise<void> => ipcRenderer.invoke('luvus:browser-takeover', id),
  onMessage: (listener: (message: Response) => void): (() => void) => {
    const receive = (_event: Electron.IpcRendererEvent, message: Response) => listener(message);
    ipcRenderer.on('luvus:message', receive);
    return () => ipcRenderer.removeListener('luvus:message', receive);
  },
}));
