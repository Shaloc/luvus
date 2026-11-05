import { app, BrowserWindow, clipboard, dialog, ipcMain, Menu, net, Notification, protocol, session, shell, type IpcMainInvokeEvent } from 'electron';
import { access, readFile, rename, stat, writeFile } from 'node:fs/promises';
import { constants } from 'node:fs';
import { isAbsolute, resolve, sep } from 'node:path';
import { pathToFileURL } from 'node:url';
import { isRequest, type Response as AppResponse } from '@luvus/app-protocol';
import { rendererMayRequest } from './browser-policy.js';
import { CoreClient, IncompatibleCoreError } from './core-client.js';
import { BrowserBridge } from './browser-bridge.js';
import { SessionAdmin, validName } from './session-admin.js';
import { Gateways } from './gateways.js';

const directory = __dirname;
const applicationScheme = 'luvus-app';
if (process.env.LUVUS_APP_USER_DATA) {
  if (!isAbsolute(process.env.LUVUS_APP_USER_DATA)) throw new Error('App user data path must be absolute');
  app.setPath('userData', process.env.LUVUS_APP_USER_DATA);
}
protocol.registerSchemesAsPrivileged([{ scheme: applicationScheme, privileges: { secure: true, standard: true, supportFetchAPI: true, corsEnabled: true } }]);
app.commandLine.appendSwitch('host-resolver-rules', 'MAP * ~NOTFOUND, EXCLUDE 127.0.0.1');
app.commandLine.appendSwitch('disable-quic');
app.commandLine.appendSwitch('force-webrtc-ip-handling-policy', 'disable_non_proxied_udp');

let window: BrowserWindow | undefined;
let core: CoreClient | undefined;
let browserBridge: BrowserBridge | undefined;
let quitting = false;
let quitCleanupComplete=false;
let direct = false, selecting = false;
let admin:SessionAdmin | undefined;
let gateways:Gateways | undefined;
let allowedOrigin = `${applicationScheme}://app`;
let retryStartup: (() => Promise<void>) | undefined;

const sessionPreference = () => resolve(app.getPath('userData'), 'desktop-session.json');
async function rememberedSession(): Promise<string | undefined> {
  try {
    if ((await stat(sessionPreference())).size > 1024) return;
    const saved = JSON.parse(await readFile(sessionPreference(), 'utf8'));
    if (saved.version === 1 && validName(saved.session) && !saved.session.startsWith('remote-')) return saved.session;
  } catch { /* Missing or invalid preferences never select an attachment. */ }
}
async function rememberSession(name: string): Promise<void> {
  const destination = sessionPreference(), temporary = destination + '.' + crypto.randomUUID();
  await writeFile(temporary, JSON.stringify({ version: 1, session: name }), { mode: 0o600, flag: 'wx' });
  await rename(temporary, destination);
}

function waitForSnapshot(client: CoreClient): Promise<void> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => finish(new Error('The selected core did not provide a desktop snapshot')), 30_000);
    const listener = (message: AppResponse) => {
      if (message.type === 'snapshot') finish();
      else if (message.type === 'error' && !message.id) finish(new Error(message.message));
    };
    function finish(error?: Error) {
      clearTimeout(timer); client.off('message', listener);
      if (error) reject(error); else resolve();
    }
    client.on('message', listener);
    void client.start().catch(finish);
  });
}

function trusted(event: IpcMainInvokeEvent): boolean {
  if (!window || event.sender !== window.webContents || event.senderFrame !== window.webContents.mainFrame) return false;
  try { return new URL(event.senderFrame.url).origin === allowedOrigin || event.senderFrame.url.startsWith(`${applicationScheme}://app/`) && allowedOrigin === `${applicationScheme}://app`; }
  catch { return false; }
}

function requireTrusted(event: IpcMainInvokeEvent): void {
  if (!trusted(event)) throw new Error('Untrusted desktop sender');
}

function external(url: unknown): void {
  if (typeof url !== 'string' || url.length > 8192) return;
  try { const parsed = new URL(url); if (['http:', 'https:'].includes(parsed.protocol)) void shell.openExternal(parsed.href); }
  catch { return; }
}

function receive(message: AppResponse): void {
  if (browserBridge?.receive(message)) return;
  if (message.type === 'error' && message.code === 'connection_failed') void browserBridge?.dispose();
  if (message.type === 'effect' && message.data && typeof message.data === 'object') {
    const data = message.data as Record<string, unknown>;
    if (message.kind === 'clipboard' && typeof data.text === 'string') {
      clipboard.writeText(data.text);
      if (typeof data.receipt === 'string') void core?.request({ type: 'clipboard_succeeded', id: crypto.randomUUID(), receipt: data.receipt }).catch(() => {});
      return;
    }
    if (message.kind === 'open_url') external(data.url);
    if (message.kind === 'notification' && typeof data.text === 'string' && Notification.isSupported()) new Notification({ title: 'Luvus', body: data.text.slice(0, 8192) }).show();
  }
  publish(message);
}

function publish(message: AppResponse): void {
  if (window && !window.webContents.isDestroyed()) window.webContents.send('luvus:message', message);
}

async function createWindow(): Promise<void> {
  const renderer = resolve(directory, '../../renderer/dist');
  const developmentUrl = app.isPackaged ? undefined : process.env.LUVUS_APP_DEV_URL ?? process.env.VITE_DEV_SERVER_URL;
  if (developmentUrl) {
    const url = new URL(developmentUrl);
    if (url.hostname !== '127.0.0.1' || url.protocol !== 'http:') throw new Error('The development renderer must use loopback HTTP');
    allowedOrigin = url.origin;
  }
  protocol.handle(applicationScheme, async request => {
    const url = new URL(request.url);
    if (url.hostname !== 'app') return new Response('Not found', { status: 404 });
    let pathname: string;
    try { pathname = decodeURIComponent(url.pathname); }
    catch { return new Response('Invalid path', { status: 400 }); }
    const file = resolve(renderer, `.${pathname === '/' ? '/index.html' : pathname}`);
    if (!file.startsWith(renderer + sep)) return new Response('Forbidden', { status: 403 });
    try {
      if (!(await stat(file)).isFile()) return new Response('Not found', { status: 404 });
      const response = await net.fetch(pathToFileURL(file).href);
      const headers = new Headers(response.headers);
      headers.set('Content-Security-Policy', "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; connect-src 'self'; object-src 'none'; frame-src 'none'; base-uri 'none'");
      return new Response(response.body, { status: response.status, headers });
    } catch { return new Response('Not found', { status: 404 }); }
  });
  session.defaultSession.setPermissionRequestHandler((_contents, _permission, callback) => callback(false));
  session.defaultSession.setPermissionCheckHandler(() => false);
  window = new BrowserWindow({
    width: 1440, height: 900, minWidth: 900, minHeight: 600, show: false,
    title: 'Luvus', backgroundColor: '#000000', titleBarStyle: 'hiddenInset',
    webPreferences: { preload: resolve(directory, 'preload.cjs'), contextIsolation: true, sandbox: true, nodeIntegration: false, webSecurity: true },
  });
  window.webContents.setWindowOpenHandler(() => ({ action: 'deny' }));
  window.webContents.on('will-navigate', (event, url) => {
    try { if (!url.startsWith(`${applicationScheme}://app/`) && new URL(url).origin !== allowedOrigin) event.preventDefault(); }
    catch { event.preventDefault(); }
  });
  window.once('ready-to-show', () => window?.show());
  window.on('close', event => { if (!quitting) { event.preventDefault(); app.quit(); } });
  window.on('closed', () => { window = undefined; });
  await window.loadURL(developmentUrl ?? `${applicationScheme}://app/`);
  const binary = (!app.isPackaged && process.env.LUVUS_APP_BINARY) || resolve(process.resourcesPath, 'luvus');
  if (!isAbsolute(binary)) throw new Error('The core binary path must be absolute');
  await access(binary, constants.X_OK);
  // Only explicit native process switches choose an SSH destination. Renderer
  // actions and inherited pane selectors cannot redirect this attachment.
  const option = (flag: string): string | undefined => {
    const i = process.argv.indexOf(flag);
    if (i < 0) return undefined;
    const value = process.argv[i + 1];
    if (!value || value.startsWith('-') || process.argv.lastIndexOf(flag) !== i) throw new Error(`Invalid ${flag} option`);
    return value;
  };
  const remote = option('--remote');
  let selectedSession = option('--session') ?? process.env.LUVUS_SESSION ?? (!remote ? await rememberedSession() : undefined) ?? 'default';
  direct = Boolean(remote);
  if (remote && !/^[a-zA-Z0-9_.][a-zA-Z0-9_.-]{0,254}$/.test(remote)) throw new Error('Invalid SSH alias');
  if (!/^[a-zA-Z0-9._-]{1,64}$/.test(selectedSession) || ['.', '..'].includes(selectedSession)) throw new Error('Invalid selected session');
  let starting = false;
  retryStartup = async () => {
    if (starting || quitting || core?.bootstrap().some(m => m.type === 'snapshot')) return;
    starting = true;
    try {
      while (!quitting && window && !window.isDestroyed()) {
        core?.off('message', receive);
        await Promise.all([core?.close(), browserBridge?.dispose(), gateways?.close()]);
        const next = new CoreClient(binary, selectedSession, process.env, remote);
        core = next;
        if (!direct) admin = new SessionAdmin(binary, selectedSession, process.env);
        if (!direct) gateways = new Gateways(binary, selectedSession, process.env);
        browserBridge = new BrowserBridge(window, next, publish);
        next.on('message', receive);
        try {
          await waitForSnapshot(next);
          if (!direct) await rememberSession(selectedSession).catch(() => {
            receive({ type: 'effect', kind: 'notice', data: { text: 'Connected, but the selected session could not be saved for the next launch.' } });
          });
          return;
        } catch (error) {
          const message = error instanceof Error ? error.message : 'Core startup failed';
          receive({ type: 'error', id: null, code: 'startup_failed', message });
          await next.close();
          if (quitting || !window || window.isDestroyed()) return;
          const incompatible = error instanceof IncompatibleCoreError;
          const answer = await dialog.showMessageBox(window, {
            type: 'warning', title: 'Luvus connection',
            message: incompatible ? 'This running session needs a newer core for the desktop app.' : 'Could not connect to the selected session.',
            detail: message + (direct ? '' : '\n\nOpen a separate desktop session with the bundled core. Existing sessions and terminals will stay running; their workspaces will not be moved.'),
            buttons: direct ? ['Cancel', 'Retry'] : ['Cancel', 'Retry', 'Open Separate Desktop Session'],
            defaultId: direct ? 1 : 2, cancelId: 0,
          });
          if (answer.response === 0) return;
          if (!direct && answer.response === 2) {
            // A fresh namespace cannot collide with an old desktop owner.
            selectedSession = 'desktop-' + crypto.randomUUID();
          }
        }
      }
    } finally { starting = false; }
  };
  await retryStartup();
}

ipcMain.handle('luvus:sessions', async (event, op:unknown, name:unknown, generation:unknown) => {
  requireTrusted(event);
  const source=core, manager=admin;
  const current = source?.bootstrap().find(m=>m.type==='snapshot');
  if (direct || !source || !manager || !current || (current.snapshot as Record<string,unknown>).server_generation!==generation || selecting || quitting) throw new Error('Session selection changed');
  if (op==='list') return manager.inventory();
  if (op==='host-add' || op==='merge') {
    selecting=true;
    try {
      if (op==='host-add' && typeof name==='string') await manager.addHost(name);
      else if (op==='merge' && (name==='on' || name==='off')) await manager.setMerge(name==='on');
      else throw new Error('Invalid host operation');
      return await manager.inventory();
    } finally { selecting=false; }
  }
  if (!validName(name) || !['create','switch','stop','delete'].includes(String(op))) throw new Error('Invalid session operation');
  selecting = true;
  let candidate:CoreClient | undefined;
  try {
    if (op==='stop' || op==='delete') {
      if (name===source.session || !window) throw new Error('Cannot change the attached session');
      const answer=await dialog.showMessageBox(window,{type:'warning',buttons:['Cancel',op==='stop'?'Stop session':'Delete session'],
        defaultId:0,cancelId:0,message:op==='stop'?`Stop “${name}” and its live terminals?`:`Delete the saved session “${name}”?`});
      if (answer.response!==1) return;
      await manager.lifecycle(op,name); return;
    }
    if (source.session===name) return;
    await manager.check(name,op==='create');
    candidate = new CoreClient(source.binary,name,process.env);
    const next = candidate;
    // Keep the current window usable after an incompatible/failed attach. Do
    // not expose either attachment to the other's browser broker or renderer.
    await new Promise<void>((resolve,reject)=>{
      const timer=setTimeout(()=>finish(new Error('New session did not provide a snapshot')),20_000);
      const listener=(m:AppResponse)=>{if(m.type==='snapshot') finish(); else if(m.type==='error' && !m.id) finish(new Error(m.message));};
      function finish(error?:Error) {clearTimeout(timer);next.off('message',listener);if(error) reject(error);else resolve();}
      next.on('message',listener);
      void next.start().catch(finish);
    });
    if (quitting || source!==core || !window || window.isDestroyed()) throw new Error('Display closed during session selection');
    source.off('message',receive);
    await Promise.all([browserBridge?.dispose(),gateways?.close()]);
    if (quitting || !window || window.isDestroyed()) throw new Error('Display closed during session selection');
    core=next; candidate=undefined;
    admin=new SessionAdmin(source.binary,name,process.env);
    gateways=new Gateways(source.binary,name,process.env);
    browserBridge=new BrowserBridge(window,next,publish);
    next.on('message',receive);
    await source.close();
    await rememberSession(name).catch(() => {});
    window.webContents.reload();
  } finally { try {await candidate?.close();} finally {selecting=false;} }
});
ipcMain.handle('luvus:gateway',async(event,op:unknown,kind:unknown,control:unknown,generation:unknown)=>{
  requireTrusted(event);
  const current=core?.bootstrap().find(m=>m.type==='snapshot');
  if(!gateways || selecting || quitting || !current || (current.snapshot as Record<string,unknown>).server_generation!==generation)throw Error('Gateway attachment changed');
  if(op==='list')return gateways.status();
  if(kind!=='web' && kind!=='uhp')throw Error('Unknown bridge');
  if(op==='start' && typeof control==='boolean')return gateways.start(kind,control);
  if(op==='stop')return gateways.stop(kind);
  if(op==='copy'){gateways.copy(kind);return gateways.status();}
  throw Error('Invalid bridge operation');
});

let addonsBusy=false;
let addonRead:{owner:SessionAdmin;promise:ReturnType<SessionAdmin['addons']>}|undefined;
ipcMain.handle('luvus:addons',async(event,op:unknown,agent:unknown,generation:unknown)=>{
  requireTrusted(event);
  const manager=admin;
  const valid=()=>manager===admin && !direct && !selecting && !quitting && core?.bootstrap().some(m=>m.type==='snapshot' && (m.snapshot as Record<string,unknown>).server_generation===generation);
  if(!manager || !valid() || !window)throw Error('Local attachment changed');
  if(addonsBusy)throw Error('Another local add-on operation is pending');
  if(op==='list'){
    if(addonRead?.owner===manager)return addonRead.promise;
    const read=addonRead={owner:manager,promise:manager.addons()};
    try{return await read.promise;}finally{if(read===addonRead)addonRead=undefined;}
  }
  if(op!=='install'&&op!=='uninstall'&&op!=='enable'&&op!=='disable')throw Error('Unknown add-on action');
  if(typeof agent!=='string'||(op==='install'||op==='uninstall'?!/^[a-z0-9_-]{1,64}$/.test(agent):agent!==''))throw Error('Invalid add-on target');
  addonsBusy=true;
  try{
    const command=op.charAt(0).toUpperCase()+op.slice(1);
    const answer=await dialog.showMessageBox(window,{type:'warning',buttons:['Cancel',command],defaultId:0,cancelId:0,
      message:agent?`${command} the local ${agent} integration?`:`${command} the bundled Luvus skill for local agents?`});
    if(!valid())throw Error('Local attachment changed');
    return await (answer.response===1?manager.changeAddon(op,agent):manager.addons());
  }finally{addonsBusy=false;}
});

ipcMain.handle('luvus:request', (event, request: unknown) => {
  requireTrusted(event);
  if (!isRequest(request)) throw new Error('Invalid App request');
  if (selecting || request.type==='detach') throw new Error('Attachment is changing');
  if (request.type === 'action' && !rendererMayRequest(request.method)) throw new Error('Browser authority stays in the native client');
  if (request.type === 'browser_result' || request.type === 'browser_closed' || request.type === 'browser_cdp_event' || request.type === 'clipboard_succeeded') throw new Error('Native completions are not renderer actions');
  if (!core) throw new Error('Core is not connected');
  return core.request(request);
});
ipcMain.handle('luvus:bootstrap', event => { requireTrusted(event); return core?.bootstrap() ?? []; });
ipcMain.handle('luvus:choose-directory', async event => {
  requireTrusted(event);
  if (!window) return null;
  const result = await dialog.showOpenDialog(window, { properties: ['openDirectory'] });
  return result.canceled ? null : result.filePaths[0] ?? null;
});

ipcMain.handle('luvus:browser-bounds', (event, id: unknown, bounds: unknown) => {
  requireTrusted(event);
  if (typeof id !== 'string' || !/^[0-9a-f]{32}$/.test(id)) throw new Error('Invalid browser id');
  if (bounds !== null && (!bounds || typeof bounds !== 'object' || Object.keys(bounds).sort().join(',') !== 'height,width,x,y')) throw new Error('Invalid browser bounds');
  browserBridge?.bounds(id, bounds as Electron.Rectangle | null);
});
ipcMain.handle('luvus:browser-status', (event, id: unknown) => {
  requireTrusted(event);
  if (typeof id !== 'string') throw new Error('Invalid browser id');
  return browserBridge?.status(id) ?? null;
});
ipcMain.handle('luvus:browser-navigate', (event, id: unknown, action: unknown, url: unknown) => {
  requireTrusted(event);
  if (typeof id !== 'string' || !/^[0-9a-f]{32}$/.test(id) || !['navigate', 'back', 'forward', 'reload'].includes(String(action))) throw new Error('Invalid browser action');
  if (action === 'navigate' && (typeof url !== 'string' || url.length > 8192)) throw new Error('Invalid browser URL');
  if (!browserBridge) throw new Error('Browser host is not connected');
  return browserBridge.navigate(id, action as 'navigate' | 'back' | 'forward' | 'reload', typeof url === 'string' ? url : undefined);
});
ipcMain.handle('luvus:browser-takeover', (event, id: unknown) => {
  requireTrusted(event);
  if (typeof id !== 'string' || !/^[0-9a-f]{32}$/.test(id)) throw new Error('Invalid browser id');
  if (!browserBridge) throw new Error('Browser host is not connected');
  return browserBridge.takeover(id);
});

app.on('before-quit', event => {
  if (quitCleanupComplete) return;
  event.preventDefault();
  if(quitting)return;
  quitting = true;
  void Promise.allSettled([core?.close(), browserBridge?.dispose(),gateways?.close()]).finally(() => {quitCleanupComplete=true;app.quit();});
});
app.on('window-all-closed', () => app.quit());
void app.whenReady().then(async () => {
  Menu.setApplicationMenu(Menu.buildFromTemplate([
    { label: 'Luvus', submenu: [{ role: 'about' }, { label: 'Retry Connection', click: () => { void retryStartup?.(); } }, { type: 'separator' }, { role: 'hide' }, { role: 'hideOthers' }, { role: 'unhide' }, { type: 'separator' }, { role: 'quit' }] },
    { role: 'editMenu' }, { role: 'viewMenu' }, { role: 'windowMenu' },
  ]));
  await createWindow();
}).catch(error => { dialog.showErrorBox('Luvus could not start', error instanceof Error ? error.message : 'Unexpected startup error'); app.quit(); });
