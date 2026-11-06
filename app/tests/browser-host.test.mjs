import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import vm from 'node:vm';
import { build } from 'esbuild';

// In-memory Electron boundary fixtures; these tests neither launch Electron nor contact a core.
const views = [];
let nextProxy;
let nextLoad;
function deferred() { let resolve; const promise = new Promise(done => { resolve = done; }); return { promise, resolve }; }
function event() { return { prevented: false, preventDefault() { this.prevented = true; } }; }
class FakeSession extends EventEmitter {
  webRequest = { onBeforeRequest: callback => { this.filter = callback; } };
  setPermissionRequestHandler(handler) { this.permissionRequest = handler; }
  setPermissionCheckHandler(handler) { this.permissionCheck = handler; }
  setDevicePermissionHandler(handler) { this.devicePermission = handler; }
  async setProxy(value) { this.proxy = value; const pending = nextProxy; nextProxy = undefined; await pending; }
  async closeAllConnections() { this.connectionsClosed = true; }
  async clearStorageData() { this.storageCleared = true; }
}
class FakeDebugger extends EventEmitter {
  attached = false;
  calls = [];
  constructor(contents) { super(); this.contents = contents; }
  attach(version) { this.attached = true; this.version = version; }
  isAttached() { return this.attached; }
  message(method, params) { this.emit('message', {}, method, params); }
  async sendCommand(method, params = {}) {
    this.calls.push({ method, params });
    const custom = this.hook?.(method, params);
    if (custom !== undefined) return custom;
    switch (method) {
      case 'Runtime.enable':
        this.message('Runtime.executionContextCreated', { context: { id: 1, uniqueId: 'main-unique', auxData: { isDefault: true, frameId: 'main' } } });
        return {};
      case 'Page.getFrameTree': return { frameTree: { frame: { id: 'main', url: this.contents.url } } };
      case 'Page.getNavigationHistory': return { currentIndex: 1, entries: [{ id: 1, url: 'https://owner.test/back' }, { id: 2, url: this.contents.url }, { id: 3, url: 'https://owner.test/forward' }] };
      case 'Page.navigate': this.contents.navigate(params.url); return {};
      case 'Page.createIsolatedWorld':
        this.message('Runtime.executionContextCreated', { context: { id: 2, uniqueId: 'tools-unique', name: 'luvus-browser-tools', auxData: { isDefault: false, frameId: 'main' } } });
        return { executionContextId: 2 };
      case 'Accessibility.getFullAXTree': return { nodes: [{ nodeId: 'ax1', backendDOMNodeId: 7, role: { value: 'textbox' }, name: { value: 'Name' } }] };
      case 'DOM.resolveNode': return { object: { objectId: 'node-object' } };
      case 'DOM.getBoxModel': return { model: { content: [0, 0, 100, 0, 100, 40, 0, 40] } };
      case 'Page.getLayoutMetrics': return { cssVisualViewport: { clientWidth: 500, clientHeight: 500 } };
      case 'Runtime.callFunctionOn': return { result: { value: true } };
      case 'Runtime.evaluate': return { result: { value: { ok: true } } };
      case 'Page.handleJavaScriptDialog': this.message('Page.javascriptDialogClosed', {}); return {};
      default: return {};
    }
  }
}
class FakeContents extends EventEmitter {
  url = 'about:blank';
  destroyed = false;
  loading = false;
  devtools = false;
  png = Buffer.from('png');
  debugger = new FakeDebugger(this);
  constructor(session) { super(); this.session = session; }
  setWebRTCIPHandlingPolicy(policy) { this.webRTC = policy; }
  setIgnoreMenuShortcuts(value) { this.ignoreMenuShortcuts = value; }
  setWindowOpenHandler(handler) { this.popup = handler; }
  isDestroyed() { return this.destroyed; }
  isDevToolsOpened() { return this.devtools; }
  isLoadingMainFrame() { return this.loading; }
  getURL() { return this.url; }
  getTitle() { return 'Owner page'; }
  navigate(url) {
    this.emit('did-start-navigation', {}, url, false, true);
    this.url = url;
    this.emit('did-navigate', {}, url);
  }
  async loadURL(url) {
    this.loadCalled = true;
    const pending = nextLoad; nextLoad = undefined;
    await pending;
    if (this.destroyed) throw new Error('destroyed');
    this.navigate(url);
  }
  async capturePage() { return { toPNG: () => this.png }; }
  close(options) { this.closeOptions = options; this.destroyed = true; this.emit('destroyed'); }
}
class FakeView {
  constructor(options) { this.options = options; this.webContents = new FakeContents(options.webPreferences.session); views.push(this); }
  setVisible(value) { this.visible = value; }
  getVisible() {return this.visible;}
  setBounds(value) { this.bounds = value; }
  getBounds() { return this.bounds ?? { x: 0, y: 0, width: 0, height: 0 }; }
}
globalThis.__browserHostTestElectron = { BrowserWindow: class {}, WebContentsView: FakeView, session: { fromPartition(partition, options) {
  const session = new FakeSession(); session.partition = partition; session.options = options; return session;
} } };
async function loadModule(name) {
  const result = await build({ entryPoints: [fileURLToPath(new URL(`../apps/desktop/src/${name}.ts`, import.meta.url))], bundle: true, write: false,
    format: 'esm', platform: 'node', plugins: [{ name: 'electron-fixture', setup(plugin) {
      plugin.onResolve({ filter: /^electron$/ }, () => ({ path: 'electron', namespace: 'fixture' }));
      plugin.onLoad({ filter: /.*/, namespace: 'fixture' }, () => ({ contents: 'export const { BrowserWindow, WebContentsView, session } = globalThis.__browserHostTestElectron;' }));
    } }] });
  return import(`data:text/javascript;base64,${Buffer.from(result.outputFiles[0].text).toString('base64')}`);
}
const { BrowserHost } = await loadModule('browser-host');
const policy = await loadModule('browser-policy');
const tools = await loadModule('browser-tools');
const target = { generation: 'owner-1', workspace_id: 'w1', tab_id: 't1', pane_id: 'p1', terminal_id: 'term1' };
function descriptor() { return { id: 'browser1', url: 'https://owner.test', proxy_port: 2345, generation: target.generation, workspace_id: target.workspace_id,
  pane_id: target.pane_id, target: { ...target }, request_id: 'launch1', lease_generation: '0' }; }
function fixture(t,onTabShortcut) {
  const notifications = [];
  const window = { isDestroyed: () => false, getContentBounds:()=>({x:10,y:20,width:1000,height:800}), webContents: {getZoomFactor:()=>1}, contentView: { addChildView() {}, removeChildView() {}, getBounds:()=>({x:0,y:0,width:1000,height:800}) } };
  const host = new BrowserHost(window, (id,reason,generation) => { assert.match(generation,/^\d+$/);notifications.push([id,reason]); },undefined,undefined,onTabShortcut);
  t.after(() => host.dispose());
  const command = (method, params = {}, generation = '1', override = {}) => host.command({ request_id: 'req1', browser_id: 'browser1', method: `browser.${method}`,
    params, lease_generation: generation, target: { ...target }, ...override });
  return { host, command, notifications, window, get view() { return views.at(-1); } };
}
async function controlled(t) { const f = fixture(t); await f.host.open(descriptor()); await f.command('control.acquire'); return f; }
async function ref(f) { const snapshot = await f.command('snapshot'); return { node: snapshot.nodes[0].node, document_revision: snapshot.document_revision }; }

test('only visible human Command-digit input can switch desktop tabs',async t=>{
  const shortcuts=[],f=fixture(t,(target,position)=>shortcuts.push({target,position}));
  await f.host.open(descriptor());const contents=f.view.webContents;
  const press=()=>{const e=event();contents.emit('before-input-event',e,{type:'keyDown',key:'2',meta:true});return e;};
  assert.equal(press().prevented,false);assert.equal(shortcuts.length,0);
  f.host.bounds('browser1',{x:0,y:0,width:400,height:300});
  assert.equal(press().prevented,true);assert.equal(shortcuts.length,1);assert.equal(shortcuts[0].position,2);
  await f.command('control.acquire');
  const reference=await ref(f);
  contents.debugger.hook=(method)=>{if(method==='Input.dispatchKeyEvent')press();};
  await f.command('act',{action:{...reference,kind:'press',key:'Enter'}});
  assert.equal(shortcuts.length,1,'agent keystroke must not escape into the desktop');
  press();assert.equal(shortcuts.length,2);assert.equal(f.host.metadata('browser1').control.state,'uncontrolled');
  contents.emit('did-start-navigation',{},'https://owner.test/slow',false,true);
  assert.equal(press().prevented,true);assert.equal(shortcuts.length,3,'loading pages must not trap Command-digit shortcuts');
});

test('only a fresh visible human gesture permits the native Save dialog, never a CDP fallback', async t=>{
  const f=fixture(t);await f.host.open(descriptor());const contents=f.view.webContents;
  f.host.bounds('browser1',{x:0,y:0,width:320,height:240});
  const download=name=>{
    const item=Object.assign(new EventEmitter(),{getFilename:()=>name,getURL:()=> 'https://owner.test/file',getTotalBytes:()=>100,
      cancel(){},setSaveDialogOptions(options){this.options=options;}});
    const e=event();contents.session.emit('will-download',e,item,contents);return {item,e};
  };
  const click=()=>contents.emit('before-mouse-event',event(),{type:'mouseDown',button:'left'});
  assert.ok(download('file.txt').e.prevented);
  click();assert.ok(download('../outside').e.prevented);
  click();const {item,e}=download('file.txt');assert.equal(e.prevented,false);assert.equal(item.options.defaultPath,'file.txt');
  assert.ok(download('again.txt').e.prevented);item.emit('done');
  click();await f.command('control.acquire');
  assert.ok(download('must-not-use-native-dialog.txt').e.prevented);
});

test('policy validates broker params, UTF-8 sizes, URLs, keys, and encoded results', () => {
  for (const url of ['file:///secret', 'chrome://settings', 'devtools://devtools', 'app://shell', 'javascript:alert(1)', 'data:text/html,x', 'https://a.test/ bad']) assert.equal(policy.allowedUrl(url), false);
  for (const url of ['about:blank', 'http://localhost:3000/', 'https://owner.test']) assert.equal(policy.allowedUrl(url), true);
  for (const url of ['file:///secret', 'javascript:alert(1)', 'about:config', 'data:text/html,x']) assert.throws(() => policy.humanBrowserAddress(url));
  assert.equal(policy.humanBrowserAddress('example.com'), 'https://example.com/');
  assert.equal(policy.humanBrowserAddress('localhost:3000/test'), 'http://localhost:3000/test');
  assert.equal(policy.humanBrowserAddress('[::1]:3000'), 'http://[::1]:3000/');
  for (const ttl_s of [0, 3601, 1.1, '2']) assert.throws(() => policy.commandParams('browser.control.acquire', { ttl_s }), /invalid_params/);
  for (const timeout_ms of [0, 20001, '20']) assert.throws(() => policy.commandParams('browser.snapshot', { timeout_ms }), /invalid_params/);
  assert.throws(() => policy.commandParams('browser.snapshot', { lease: 'secret' }), /invalid_params/);
  assert.throws(() => policy.commandParams('browser.screenshot', { path: '/tmp/image.png' }), /invalid_params/);
  assert.throws(() => policy.commandParams('browser.evaluate', { expression: '界'.repeat(12000) }), /invalid_params/);
  assert.throws(() => policy.commandParams('browser.wait', { condition: { text: 'a', load: true } }), /invalid_params/);
  assert.throws(() => policy.commandParams('Target.createTarget', {}), /unsupported/);
  assert.throws(() => policy.generation('01'), /invalid_params/);
  assert.throws(() => policy.bounded({ text: '界'.repeat(500000) }), /limit_exceeded/);
  assert.equal(policy.keyStroke('Control+Shift+a').modifiers, 10);
  for (const key of ['Control+Control+a', 'Super+A', 'F13', 'Enter;evil()', '']) assert.throws(() => policy.keyStroke(key), /invalid_params/);
});

test('open keeps isolated fixed-proxy security, denies permissions and navigation, and waits for bounds', async t => {
  const f = fixture(t); await f.host.open(descriptor());
  const { webPreferences: prefs } = f.view.options;
  assert.deepEqual([prefs.nodeIntegration, prefs.contextIsolation, prefs.sandbox, prefs.webSecurity, prefs.navigateOnDragDrop], [false, true, true, true, false]);
  const s = prefs.session;
  assert.deepEqual(s.proxy, { mode: 'fixed_servers', proxyRules: 'http://127.0.0.1:2345', proxyBypassRules: '<-loopback>' });
  assert.equal(s.partition, 'luvus-browser-browser1'); assert.equal(s.options.cache, false);
  assert.equal(s.permissionCheck(), false); assert.equal(s.devicePermission(), false);
  s.permissionRequest(null, 'camera', value => assert.equal(value, false));
  const download = event(); s.emit('will-download', download); assert.equal(download.prevented, true);
  assert.deepEqual(f.view.webContents.popup({ url: 'https://owner.test' }), { action: 'deny' });
  for (const name of ['will-navigate', 'will-redirect']) { const e = event(); f.view.webContents.emit(name, e, 'file:///secret'); assert.equal(e.prevented, true); }
  const frame = { ...event(), url: 'devtools://x' }; f.view.webContents.emit('will-frame-navigate', frame); assert.equal(frame.prevented, true);
  s.filter({ url: 'data:text/html,bad', resourceType: 'subFrame' }, result => assert.equal(result.cancel, true));
  s.filter({ url: 'wss://owner.test/socket', resourceType: 'webSocket' }, result => assert.equal(result.cancel, false));
  assert.equal(f.view.webContents.webRTC, 'disable_non_proxied_udp');
  assert.equal(f.view.webContents.ignoreMenuShortcuts, true);
  assert.deepEqual(f.view.webContents.debugger.calls.find(call => call.method === 'Page.setInterceptFileChooserDialog').params, { enabled: true });
  assert.equal(f.view.visible, false);
  f.host.bounds('browser1', { x: 10, y: 20, width: 400, height: 300 }); assert.equal(f.view.visible, true);
  f.host.bounds('browser1', { x: 0, y: 0, width: 1002, height: 2 });
  assert.deepEqual(f.view.bounds,{x:0,y:0,width:1000,height:2});
  for(const width of [-1,NaN,Infinity]) assert.throws(() => f.host.bounds('browser1', { x: 0, y: 0, width, height: 2 }), /Invalid browser bounds/);
  f.window.webContents.getZoomFactor=()=>1.25;
  f.host.bounds('browser1',{x:10.4,y:20.8,width:200,height:100});
  assert.deepEqual(f.view.bounds,{x:13,y:26,width:250,height:125});
  assert.equal(f.host.metadata('browser1').control.state, 'uncontrolled');
  await assert.rejects(f.command('snapshot'), /control_revoked/);
});

test('every target field and generation is fenced; launch replay never grants control', async t => {
  const f = await controlled(t);
  for (const key of Object.keys(target)) await assert.rejects(f.command('snapshot', {}, '1', { target: { ...target, [key]: 'other' } }), /route_changed/);
  await assert.rejects(f.command('snapshot', {}, '0'), /control_revoked/);
  await assert.rejects(f.command('control.acquire', {}, '1'), /control_denied/);
  f.host.takeControl('browser1');
  assert.equal(f.host.metadata('browser1').control.state, 'uncontrolled');
  await assert.rejects(f.host.open(descriptor()), /control_revoked/);
  await assert.rejects(f.command('control.acquire', {}, '1'), /control_denied/);
  await assert.rejects(f.command('control.acquire', {}, '2'), /control_denied/);
  await f.command('control.acquire', {}, '3');
  assert.equal(f.host.metadata('browser1').control.state, 'acquired');
  assert.deepEqual(f.notifications, [['browser1', 'control_revoked']]);
});

test('takeover before an in-transit acquisition arrives fences that generation and notifies the owner', async t => {
  const f = fixture(t); await f.host.open(descriptor());
  f.host.takeControl('browser1');
  await assert.rejects(f.command('control.acquire', {}, '1'), /control_denied/);
  assert.deepEqual(f.notifications, [['browser1', 'control_revoked']]);
  await f.command('control.release', { reason: 'control_revoked' }, '2');
  await f.command('control.acquire', {}, '3');
});

test('descriptor target is copied and cannot be changed by its caller', async t => {
  const f = fixture(t); const d = descriptor(); await f.host.open(d); d.target.tab_id = 'mutated';
  await f.command('control.acquire');
  await f.command('snapshot');
});

test('stale release and late completion cannot cancel a new lease; lifetime close remains terminal', async t => {
  const f = await controlled(t);
  await f.command('control.release', {}, '2');
  await assert.rejects(f.command('snapshot', {}, '2'), /control_revoked/);
  await f.command('control.acquire', {}, '3');
  await f.command('control.release', { reason: 'lease_expired' }, '2');
  assert.equal(f.host.metadata('browser1').lease_generation, '3');
  assert.equal(f.host.retireLostReply('browser1','1'),false);
  await f.command('snapshot', {}, '3');
  await f.command('close', { reason: 'route_changed' }, '1');
  await f.command('close', { reason: 'route_changed' }, '1');
  await assert.rejects(f.command('control.acquire', {}, '4'), /browser_closed/);
  await assert.rejects(f.host.open(descriptor()), /browser_closed/);
  assert.equal(f.notifications.length, 0);
});

test('closing during proxy setup and load prevents late resurrection', async t => {
  for (const phase of ['proxy', 'load']) {
    const f = fixture(t); const pending = deferred();
    if (phase === 'proxy') nextProxy = pending.promise; else nextLoad = pending.promise;
    const open = f.host.open(descriptor());
    const rejected = assert.rejects(open, /browser_closed/);
    if (phase === 'proxy') f.view.options.webPreferences.session.filter({ url: 'https://owner.test', resourceType: 'mainFrame' }, result => assert.equal(result.cancel, true));
    if (phase === 'load') {
      // Synchronize on the actual load dispatch, not a timing sleep.
      while (!f.view.webContents.loadCalled) await new Promise(resolve => setImmediate(resolve));
    }
    await Promise.all([f.host.close('browser1'), f.host.close('browser1')]);
    pending.resolve(); await rejected;
    assert.equal(f.view.webContents.destroyed, true);
    assert.equal(f.view.webContents.closeOptions.waitForBeforeUnload, false);
    if (phase === 'proxy') assert.equal(f.view.webContents.loadCalled, undefined);
  }
});

test('external view destruction cleans up without reading a destroyed WebContents session', async t => {
  const f = await controlled(t); const contents = f.view.webContents;
  Object.defineProperty(contents, 'session', { get() { throw new Error('destroyed contents'); } });
  contents.destroyed = true; contents.emit('destroyed');
  await f.host.close('browser1');
  await assert.rejects(f.command('snapshot'), /browser_closed/);
  assert.deepEqual(f.notifications, [['browser1', 'browser_closed']]);
});

test('snapshot offers bounded DOM refs and invalidates them on replacement and main navigation', async t => {
  const f = await controlled(t);
  const first = await ref(f); const second = await ref(f);
  await assert.rejects(f.command('act', { action: { ...first, kind: 'click' } }), /stale_reference/);
  await assert.rejects(f.command('act', { action: { ...second, node: 'unoffered', kind: 'click' } }), /stale_reference/);
  f.view.webContents.emit('did-start-navigation', {}, 'https://owner.test/next', false, true);
  f.view.webContents.emit('did-navigate', {}, 'https://owner.test/next');
  await assert.rejects(f.command('act', { action: { ...second, kind: 'click' } }), /stale_reference/);
  f.view.webContents.debugger.hook = method => method === 'Accessibility.getFullAXTree' ? { nodes: Array.from({ length: 900 }, (_, i) => ({ nodeId: `${i}`, backendDOMNodeId: i + 1, name: { value: '界'.repeat(1000) } })) } : undefined;
  const snapshot = await f.command('snapshot');
  assert.ok(snapshot.nodes.length <= 512); assert.equal(snapshot.truncated, true);
  assert.ok(Buffer.byteLength(JSON.stringify(snapshot)) <= policy.MAX_RESULT_BYTES);
});

test('fill/select/scroll use static code and distinct arguments; file uploads and extra params are not exposed', async t => {
  const f = await controlled(t); const reference = await ref(f);
  const injection = "'); globalThis.pwned = true; //";
  await f.command('act', { action: { ...reference, kind: 'fill', text: injection } });
  await f.command('act', { action: { ...reference, kind: 'select', values: [injection] } });
  await f.command('act', { action: { ...reference, kind: 'scroll', x: -1000, y: 1000 } });
  const calls = f.view.webContents.debugger.calls.filter(call => call.method === 'Runtime.callFunctionOn' && ['fill', 'select', 'scroll'].includes(call.params.arguments?.[0].value));
  assert.equal(calls.length, 3);
  for (const call of calls) { assert.equal(call.params.functionDeclaration, tools.NODE_ACTION); assert.ok(!call.params.functionDeclaration.includes(injection)); }
  assert.equal(calls[0].params.arguments[1].value, injection);
  assert.deepEqual(calls[1].params.arguments[1].value, [injection]);
  await assert.rejects(f.command('act', { action: { ...reference, kind: 'scroll', x: 100001, y: 0 } }), /invalid_params/);
  await assert.rejects(f.command('act', { action: { ...reference, kind: 'fill', text: '', path: '/secret' } }), /invalid_params/);
  assert.match(tools.NODE_ACTION, /'file'/);
});

test('click and press dispatch bounded CDP input without treating it as human takeover', async t => {
  const f = await controlled(t); const reference = await ref(f); const contents = f.view.webContents;
  const physical = event(); contents.emit('before-input-event', physical, { type: 'keyDown', key: 'a' }); assert.equal(physical.prevented, true);
  const mouse = event(); contents.emit('before-mouse-event', mouse, { type: 'mouseDown', x: 50, y: 20 }); assert.equal(mouse.prevented, true);
  contents.debugger.hook = (method, params) => {
    if (method === 'Input.dispatchKeyEvent' || method === 'Input.dispatchMouseEvent') {
      const key = method === 'Input.dispatchKeyEvent';
      const input = key ? { type: params.type, key: params.key, control: true, shift: true } : {
        type: params.type === 'mousePressed' ? 'mouseDown' : 'mouseUp', button:params.button, globalX: 10 + params.x, globalY: 20 + params.y };
      const synthetic = event(); contents.emit(key ? 'before-input-event' : 'before-mouse-event', synthetic, input); assert.equal(synthetic.prevented, false);
      const duplicate = event(); contents.emit(key ? 'before-input-event' : 'before-mouse-event', duplicate, input); assert.equal(duplicate.prevented, true);
    }
  };
  await f.command('act', { action: { ...reference, kind: 'press', key: 'Control+Shift+a' } });
  await f.command('act', { action: { ...reference, kind: 'click' } });
  assert.equal(f.notifications.length, 0);
  f.host.takeControl('browser1');
  const human = event(); contents.emit('before-input-event', human, { type: 'keyDown', key: 'a' }); assert.equal(human.prevented, false);
});

test('uncovered wheel/gesture inputs revoke visibly rather than leaving concurrent ownership', async t => {
  const f = await controlled(t);
  f.view.webContents.emit('input-event', {}, { type: 'mouseWheel' });
  assert.equal(f.host.metadata('browser1').control.state, 'uncontrolled');
  assert.deepEqual(f.notifications, [['browser1', 'control_revoked']]);
  await assert.rejects(f.command('snapshot'), /control_revoked/);
});

test('revocation or document changes after an asynchronous boundary prevent subsequent effects', async t => {
  for (const reason of ['takeover', 'navigation']) {
    const f = await controlled(t); const reference = await ref(f);
    f.view.webContents.debugger.hook = method => {
      if (method === 'DOM.getBoxModel') {
        if (reason === 'takeover') f.host.takeControl('browser1');
        else f.view.webContents.emit('did-start-navigation', {}, 'https://owner.test/next', false, true);
      }
    };
    await assert.rejects(f.command('act', { action: { ...reference, kind: 'click' } }), reason === 'takeover' ? /control_revoked/ : /stale_reference/);
    assert.equal(f.view.webContents.debugger.calls.filter(call => call.method.startsWith('Input.')).length, 0);
  }
});

test('navigation commands validate URLs/history and use only fixed debugger methods', async t => {
  const f = await controlled(t);
  await f.command('navigate', { url: 'https://owner.test/next' });
  await f.command('back'); await f.command('forward'); await f.command('reload');
  await assert.rejects(f.command('navigate', { url: 'chrome://settings' }), /invalid_params/);
  f.view.webContents.debugger.hook = method => method === 'Page.getNavigationHistory' ? { currentIndex: 1, entries: [{ id: 1, url: 'file:///secret' }] } : undefined;
  await assert.rejects(f.command('back'), /unsupported/);
  assert.equal(f.view.webContents.debugger.calls.filter(call => call.method === 'Page.navigateToHistoryEntry').length, 2);
});

test('evaluate targets only the main page unique context, releases objects, and caps results', async t => {
  const f = await controlled(t); const debug = f.view.webContents.debugger;
  debug.message('Runtime.executionContextCreated', { context: { id: 99, uniqueId: 'iframe-unique', auxData: { isDefault: true, frameId: 'child' } } });
  assert.deepEqual((await f.command('evaluate', { expression: 'Promise.resolve({ok:true})' })).value, { ok: true });
  const call = debug.calls.find(call => call.method === 'Runtime.evaluate');
  assert.equal(call.params.uniqueContextId, 'main-unique'); assert.equal(call.params.returnByValue, true); assert.equal(call.params.awaitPromise, true);
  assert.ok(call.params.timeout <= 20000); assert.ok(debug.calls.some(call => call.method === 'Runtime.releaseObjectGroup'));
  debug.hook = method => method === 'Runtime.evaluate' ? { result: { value: 'x'.repeat(policy.MAX_RESULT_BYTES) } } : undefined;
  await assert.rejects(f.command('evaluate', { expression: 'large()' }), /limit_exceeded/);
});

test('screenshot bounds the base64 response rather than only raw PNG bytes', async t => {
  const f = await controlled(t);
  const result = await f.command('screenshot'); assert.equal(result.mime_type, 'image/png'); assert.equal(result.base64, Buffer.from('png').toString('base64'));
  await assert.rejects(f.command('screenshot', { max_bytes: 4 }), /limit_exceeded/);
  f.view.webContents.png = Buffer.alloc(800000);
  await assert.rejects(f.command('screenshot'), /limit_exceeded/);
});

test('load wait observes loading events and removes listeners on completion', async t => {
  const f = await controlled(t); const contents = f.view.webContents;
  contents.loading = true;
  const waiting = f.command('wait', { condition: { load: true } });
  contents.emit('did-start-navigation', {}, 'https://owner.test/next', false, true);
  contents.emit('did-navigate', {}, 'https://owner.test/next');
  contents.loading = false; contents.emit('did-stop-loading');
  assert.equal((await waiting).matched, true);
  assert.equal(contents.listenerCount('did-stop-loading'), 0);
  assert.equal(contents.listenerCount('did-finish-load'), 0);
});

test('timeout revokes a stalled page observation and cleans up its isolated observer', async t => {
  const f = await controlled(t); const debug = f.view.webContents.debugger;
  debug.hook = (method, params) => method === 'Runtime.callFunctionOn' && params.functionDeclaration === tools.WAIT_FOR_PAGE ? new Promise(() => {}) : undefined;
  await assert.rejects(f.command('wait', { timeout_ms: 25, condition: { text: 'not present' } }), /timeout/);
  assert.equal(f.host.metadata('browser1').control.state, 'uncontrolled');
  assert.equal(f.notifications.length, 1);
  assert.ok(debug.calls.some(call => call.params.functionDeclaration === tools.CANCEL_WAIT));
  assert.ok(debug.calls.some(call => call.method === 'Runtime.terminateExecution'));
});

test('navigation cancels text observation in its original unique world; release removes load listeners', async t => {
  const f = await controlled(t); const debug = f.view.webContents.debugger; const entered = deferred();
  debug.hook = (method, params) => {
    if (method === 'Runtime.callFunctionOn' && params.functionDeclaration === tools.WAIT_FOR_PAGE) {
      assert.equal(params.uniqueContextId, 'tools-unique'); entered.resolve(); return new Promise(() => {});
    }
  };
  const waiting = f.command('wait', { condition: { text: 'not here' } });
  const rejected = assert.rejects(waiting, /stale_reference/);
  await entered.promise;
  f.view.webContents.emit('did-start-navigation', {}, 'https://owner.test/next', false, true);
  await rejected;
  const cancel = debug.calls.find(call => call.params.functionDeclaration === tools.CANCEL_WAIT);
  assert.equal(cancel.params.uniqueContextId, 'tools-unique');
  f.view.webContents.loading = true;
  const loading = f.command('wait', { condition: { load: true } });
  const released = assert.rejects(loading, /control_revoked/);
  await f.command('control.release', { reason: 'control_released' }, '2');
  await released;
  assert.equal(f.view.webContents.listenerCount('did-stop-loading'), 0);
});

test('DevTools or debugger detach cancels in-flight commands and requires fresh acquisition', async t => {
  for (const trigger of ['detach', 'devtools']) {
    const f = await controlled(t); const contents = f.view.webContents; const entered = deferred();
    contents.debugger.hook = method => {
      if (method === 'Runtime.evaluate') { entered.resolve(); return new Promise(() => {}); }
    };
    const evaluating = f.command('evaluate', { expression: 'new Promise(()=>{})' });
    const rejected = assert.rejects(evaluating, /control_revoked/);
    await entered.promise;
    if (trigger === 'detach') { contents.debugger.attached = false; contents.debugger.emit('detach', {}, 'target_closed'); }
    else { contents.devtools = true; contents.emit('devtools-opened'); }
    await rejected;
    assert.equal(f.notifications.length, 1);
    await assert.rejects(f.command('snapshot'), /control_revoked/);
    if (trigger === 'devtools') await assert.rejects(f.command('control.acquire', {}, '2'), /control_denied/);
  }
});

test('lease expiry revokes without idle polling or replay acquisition', async t => {
  const f = fixture(t); await f.host.open(descriptor());
  t.mock.timers.enable({ apis: ['setTimeout'] });
  await f.command('control.acquire', { ttl_s: 1 });
  t.mock.timers.tick(1000);
  await assert.rejects(f.command('snapshot'), /control_revoked/);
  assert.equal(f.notifications.length, 1);
  await assert.rejects(f.command('control.acquire', {}, '1'), /control_denied/);
});

test('dialog metadata and prompt arguments use Page.handleJavaScriptDialog', async t => {
  const f = await controlled(t); const debug = f.view.webContents.debugger;
  debug.message('Page.javascriptDialogOpening', { type: 'prompt', message: 'Name?', defaultPrompt: 'a' });
  assert.deepEqual(f.host.metadata('browser1').dialog, { type: 'prompt', message: 'Name?', default_prompt: 'a' });
  await f.command('dialog', { accept: true, prompt_text: "');bad()" });
  assert.deepEqual(debug.calls.find(call => call.method === 'Page.handleJavaScriptDialog').params, { accept: true, promptText: "');bad()" });
  assert.equal(f.host.metadata('browser1').dialog, null);
});

test('page text observer is event-driven and disconnects on match, cancellation, and timeout', async () => {
  for (const outcome of ['match', 'cancel', 'timeout']) {
    let observer; let timer; let disconnected = false; let cleared = false;
    const listeners = new Map();
    const page = vm.createContext({ document: { body: { textContent: '' } },
      MutationObserver: class { constructor(callback) { observer = callback; } observe() {} disconnect() { disconnected = true; } },
      addEventListener: (name, listener) => listeners.set(name, listener), removeEventListener: name => listeners.delete(name),
      setTimeout: callback => { timer = callback; return 1; }, clearTimeout: () => { cleared = true; } });
    const waiting = vm.runInContext(`(${tools.WAIT_FOR_PAGE})`, page)('token', { text: 'ready' }, 100);
    if (outcome === 'match') { page.document.body.textContent = 'ready'; observer(); }
    else if (outcome === 'cancel') vm.runInContext(`(${tools.CANCEL_WAIT})`, page)('token');
    else timer();
    assert.equal(await waiting, outcome === 'match'); assert.equal(disconnected, true); assert.equal(cleared, true);
    assert.equal(listeners.size, 0); assert.equal(page.__luvusBrowserWaits.size, 0);
  }
});
