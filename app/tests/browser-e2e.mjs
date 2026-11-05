import { useEnglishUi } from './device-language.mjs';
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { createServer, request as httpRequest } from 'node:http';
import { mkdtemp, mkdir, readFile, realpath, symlink, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { _electron } from 'playwright-core';

const execute = promisify(execFile);
const root = await realpath(resolve(dirname(fileURLToPath(import.meta.url)), '../..'));
const bundle=process.env.LUVUS_TEST_BUNDLE ? await realpath(process.env.LUVUS_TEST_BUNDLE) : null;
const binary = bundle ? join(bundle,'Contents/Resources/luvus') : join(root, 'target/debug/luvus');
if (process.env.LUVUS_ENV) throw new Error('Run the fixture outside a managed Luvus pane');
const remote = process.argv.includes('--remote-substitute');
const home = await mkdtemp(join(root, remote ? 'target/remote-smoke-app-' : 'target/app-e2e-browser-'));
const workspace = join(home, 'workspace');
await mkdir(workspace);
await mkdir(join(home, 'electron'));
await mkdir(join(home, 'tmp'));
await writeFile(join(home, '.isolated-app'), home);
await writeFile(join(home, 'config.json'), JSON.stringify({ check_updates: false, language: 'en' }));
const listen = server => new Promise(resolve => server.listen(0, '127.0.0.1', () => resolve(server.address().port)));
const close = server => new Promise(resolve => { server.closeAllConnections(); server.close(resolve); });
const attachment = Buffer.from(Array.from({ length: 150_017 }, (_, i) => (i * 7) % 256));
const content = createServer((req, res) => {
  if (req.url === '/attachment' || req.url === '/slow-download') {
    res.writeHead(200, { 'Content-Type': 'application/octet-stream', 'Content-Length': attachment.length,
      'Content-Disposition': 'attachment; filename="' + (req.url === '/attachment' ? 'artifact.bin' : 'incomplete.bin') + '"' });
    if (req.url === '/attachment') res.end(attachment);
    else res.write(attachment.subarray(0, 4096));
    return;
  }
  res.setHeader('Content-Type', 'text/html; charset=utf-8');
  res.end('<!doctype html><title>Owner browser fixture</title><style>body{font:18px system-ui;padding:32px;color:#eaeaea;background:#141414}input,button{font:inherit;padding:8px;margin:8px}</style><h1>Owner network</h1><label>Name<input id="name" aria-label="Name"></label><button id="greet" onclick="document.getElementById(\'result\').textContent=\'Hello \'+document.getElementById(\'name\').value">Greet</button><p id="result">Waiting</p><label>Owner files<input type="file" id="upload" multiple></label>');
});
const contentPort = await listen(content);
const proxyRequests = [];
const ownerProxy = createServer((request, response) => {
  proxyRequests.push({ url: request.url, authorization: request.headers['proxy-authorization'] });
  const destination = new URL(request.url);
  const upstream = httpRequest({ hostname: '127.0.0.1', port: contentPort, path: destination.pathname, method: request.method, headers: { host: destination.host } }, result => {
    response.writeHead(result.statusCode ?? 502, result.headers);
    result.pipe(response);
  });
  upstream.on('error', () => { response.writeHead(502); response.end(); });
  request.pipe(upstream);
});
const ownerProxyPort = await listen(ownerProxy);
let wrongProxyRequests = 0;
const displayProxy = createServer((_request, response) => { wrongProxyRequests++; response.writeHead(502); response.end('Wrong display proxy'); });
const displayProxyPort = await listen(displayProxy);
const env = Object.fromEntries(Object.entries(process.env).filter(([name]) => !name.startsWith('LUVUS_') && name !== 'ELECTRON_RUN_AS_NODE'));
for (const key of ['HTTP_PROXY', 'HTTPS_PROXY', 'ALL_PROXY', 'NO_PROXY', 'http_proxy', 'https_proxy', 'all_proxy', 'no_proxy']) env[key] = '';
Object.assign(env, { HOME: home, TMPDIR: join(home, 'tmp'), XDG_CONFIG_HOME: join(home, 'config'), XDG_DATA_HOME: join(home, 'data'), XDG_CACHE_HOME: join(home, 'cache'), XDG_STATE_HOME: join(home, 'state'), LUVUS_HOME: home, LUVUS_SESSION: 'browser-test', LUVUS_APP_BINARY: binary, LUVUS_APP_USER_DATA: join(home, 'electron'), SHELL: '/bin/sh', TERM: 'xterm-256color' });
const ownerEnv = { ...env, HTTP_PROXY: `http://fixture-user:fixture-pass@127.0.0.1:${ownerProxyPort}`,NO_PROXY:'127.0.0.1,localhost,::1' };
const displayEnv = { ...env, HTTP_PROXY: `http://127.0.0.1:${displayProxyPort}` };
if (remote) {
  // Reuse the repo's exact-binary, marker-checked SSH substitute. Do not
  // contact an actual SSH host or enable the macOS Remote Login service.
  for (const name of ['remote-home', 'remote-state', 'local-state', 'local-home', 'bin']) await mkdir(join(home, name));
  await writeFile(join(home, '.isolated-smoke'), home, { flag: 'wx' });
  await symlink(join(root, 'scripts/test-remote-sessions.py'), join(home, 'bin/ssh'));
  await writeFile(join(home, 'browser-http.json'), JSON.stringify([displayProxyPort, contentPort]));
  await writeFile(join(home, 'browser-proxy-ports.json'), JSON.stringify({ 'owner-proxy.invalid': ownerProxyPort }));
  await writeFile(join(home, 'remote-state/config.json'), JSON.stringify({ check_updates: false, language: 'en' }));
  Object.assign(displayEnv, { LUVUS_HOME: join(home, 'local-state'), HOME: join(home, 'local-home'),
    LUVUS_SMOKE_ROOT: home, LUVUS_SMOKE_BINARY: binary, PATH: join(home, 'bin') + ':' + env.PATH });
  Object.assign(ownerEnv, { LUVUS_HOME: join(home, 'remote-state'), HOME: join(home, 'remote-home'),
    HTTP_PROXY: `http://fixture-user:fixture-pass@owner-proxy.invalid:${ownerProxyPort}`, NO_PROXY: 'localhost' });
}
const command = (...args) => execute(binary, ['--session', 'browser-test', ...args], { env: ownerEnv, cwd: workspace, timeout: 30000, maxBuffer: 2 * 1024 * 1024 });
const browser = async (...args) => {
  const response = await command('browser', ...args).catch(error => {
    if (error.stdout) return { stdout: error.stdout };
    throw new Error('Browser CLI did not return a response');
  });
  const result = JSON.parse(response.stdout);
  if (result.error) throw new Error(JSON.stringify(result.error));
  return result.result ?? result;
};
let desktop;
let started = false;
let page;
try {
  await command('server', 'start'); started = true;
  await command('workspace', 'open', workspace);
  if(bundle)delete displayEnv.LUVUS_APP_BINARY;
  desktop = await _electron.launch({...(bundle?{executablePath:join(bundle,'Contents/MacOS/Luvus')}:{}),args: [...(bundle?[]:[join(root,'app/apps/desktop/dist/main.cjs')]),
    ...(remote ? ['--remote', 'fake-dev'] : []),'--session','browser-test'], env: displayEnv,
    cwd: remote ? join(home, 'local-home') : workspace, timeout: 45000 });
  page = await desktop.firstWindow();
  await useEnglishUi(page);
  await page.locator('canvas').first().waitFor({ state: 'visible', timeout: 20000 });
  await page.locator('canvas').first().click();
  await page.keyboard.type("printf 'terminal-\\142aseline\\n'");
  await page.keyboard.press('Enter');
  const painted = () => page.waitForFunction(() =>
    document.querySelector('.terminal-accessible')?.textContent.includes('terminal-baseline'), null, { timeout: 5000 });
  await painted();
  await page.evaluate(() => { window.__browserEvents = []; window.luvus.onMessage(message => window.__browserEvents.push(message)); });
  await page.getByRole('button', { name: 'New browser tab', exact: true }).click();
  await page.waitForFunction(() => document.querySelector('[aria-label="Browser address"]')?.disabled === false, null, { timeout: 30000 });
  assert.equal(await page.getByLabel('Browser address').inputValue(), '');
  await page.getByLabel('Browser address').fill(`http://owner-only.invalid:${contentPort}/`);
  await page.getByLabel('Browser address').press('Enter');
  await page.getByRole('tab',{name:'Owner browser fixture',exact:true}).waitFor();
  assert.equal(await page.locator('.global-error').count(),0);
  const inventory = await browser('list');
  assert.equal(await page.getByRole('tab').count(), 2, 'browser is a peer tab');
  await page.getByRole('tab', {name:'1',exact:true}).click();
  await painted();
  await page.getByRole('tab', {name:'Owner browser fixture',exact:true}).click();
  await page.waitForFunction(() => document.querySelector('[aria-label="Browser address"]')?.disabled === false);
  const target = inventory.browsers.find(value => value.state === 'ready');
  assert.ok(target, 'a ready display browser must be registered');
  assert.ok(proxyRequests.length > 0, 'owner proxy was not used');
  assert.equal(wrongProxyRequests, 0, 'display proxy leaked into owner browsing');
  assert.equal(proxyRequests[0].authorization, `Basic ${Buffer.from('fixture-user:fixture-pass').toString('base64')}`);
  const acquired = await browser('control', 'acquire', '--browser', target.browser_id, '--ttl', '30');
  assert.ok(acquired.lease);
  const control = ['--browser', target.browser_id, '--lease', acquired.lease];
  const snapshot = await browser('snapshot', ...control);
  const document = snapshot.data;
  assert.equal(document.title, 'Owner browser fixture');
  const name = document.nodes.find(node => node.role === 'textbox' && node.name === 'Name');
  const button = document.nodes.find(node => node.role === 'button' && node.name === 'Greet');
  assert.ok(name && button, 'snapshot must offer bounded actionable references');
  await browser('act', ...control, '--action', JSON.stringify({ kind: 'fill', node: name.node, document_revision: document.document_revision, text: 'Luvus' }));
  await browser('act', ...control, '--action', JSON.stringify({ kind: 'click', node: button.node, document_revision: document.document_revision }));
  const result = await browser('evaluate', ...control, '--expression', 'document.getElementById("result").textContent');
  assert.equal(result.data.value, 'Hello Luvus');
  await page.getByRole('tab', {name:'1',exact:true}).click();
  await painted();
  await page.getByRole('tab', {name:'Owner browser fixture',exact:true}).click();
  await page.waitForFunction(() => document.querySelector('[aria-label="Browser address"]')?.disabled === false);
  assert.equal((await browser('evaluate', ...control, '--expression', 'document.getElementById("result").textContent')).data.value, 'Hello Luvus');
  const isolation = await browser('evaluate', ...control, '--expression', '({node:typeof require,bridge:typeof window.luvus})');
  assert.deepEqual(isolation.data.value, { node: 'undefined', bridge: 'undefined' });
  const screenshot = await browser('screenshot', ...control);
  await writeFile(join(home, 'browser.png'), Buffer.from(screenshot.data.base64, 'base64'));
  const saved = await browser('screenshot', ...control, '--output', 'owner.png');
  assert.equal(saved.data.path, join(workspace, 'owner.png'));
  const png = await readFile(saved.data.path);
  assert.equal(saved.data.bytes, png.length);
  assert.deepEqual(png.subarray(0, 8), Buffer.from('89504e470d0a1a0a', 'hex'));
  await assert.rejects(() => browser('screenshot', ...control, '--output', 'owner.png'), /file_exists/);
  assert.deepEqual(await readFile(saved.data.path), png);
  await symlink(home, join(workspace, 'outside'));
  await assert.rejects(() => browser('screenshot', ...control, '--output', 'outside/blocked.png'), /file_denied/);
  await assert.rejects(() => browser('screenshot', ...control, '--output', '../blocked.png'), /file_denied/);
  await assert.rejects(readFile(join(home, 'blocked.png')), { code: 'ENOENT' });
  if (remote) {
    const forwarded = JSON.parse(await readFile(join(home, 'browser-ssh-forward'), 'utf8'));
    assert.ok(forwarded.includes('-D') && !forwarded.includes('-R'));
    const localUrl = `http://localhost:${displayProxyPort}/`;
    await browser('navigate', ...control, '--url', localUrl);
    await browser('wait', ...control, '--condition', JSON.stringify({ load: true }));
    await browser('wait', ...control, '--condition', JSON.stringify({ text: 'Owner network' }));
    assert.equal((await browser('snapshot', ...control)).data.url, localUrl);
    assert.equal(wrongProxyRequests, 0, 'NO_PROXY leaked into display loopback');
    console.log('PASS: direct App attachment via SSH substitute, owner HTTP proxy and owner localhost through -D (no real SSH)');
  }
  await page.screenshot({ path: join(home, 'browser-shell.png') });
  await page.getByRole('button', { name: 'Take control', exact: true }).click();
  const after = await browser('control', 'status', '--browser', target.browser_id);
  assert.equal(after.control.state, 'uncontrolled');
  if (!remote) {
    const dst=join(home,'chosen-artifact.bin');
    const receipt=await desktop.evaluate(async({webContents},{dst,port})=>{
      const guest=webContents.getAllWebContents().find(w=>w.getURL().startsWith('http://owner-only.invalid:'+port+'/'));
      if(!guest)throw Error('missing visible guest');
      const link=await guest.executeJavaScript(`(()=>{const a=document.createElement('a');a.href='/attachment';a.download='artifact.bin';a.textContent='Get attachment';a.style='position:fixed;top:15px;left:15px;z-index:1';document.body.appendChild(a);const r=a.getBoundingClientRect();return {x:Math.floor(r.x+r.width/2),y:Math.floor(r.y+r.height/2)}})()`);
      return new Promise((resolve,reject)=>{
        const clean=()=>{clearTimeout(timer);guest.session.off('will-download',onDownload);};
        const timer=setTimeout(()=>{clean();reject(Error('native download not observed'));},8000);
        const onDownload=(event,item)=>{
          if(event.defaultPrevented){clean();reject(Error('visible human download denied'));return;}
          const options=item.getSaveDialogOptions();
          if(!options?.title?.startsWith('Save download from ') || options.defaultPath!=='artifact.bin'){event.preventDefault();clean();reject(Error('missing scoped native save options'));return;}
          // Substitute only this test's destination after the product has selected
          // the OS chooser. Never open a dialog over the user's other apps.
          item.setSavePath(dst);
          item.once('done',(_e,state)=>{clean();resolve({state,name:options.defaultPath});});
        };
        guest.session.on('will-download',onDownload);
        guest.focus();
        guest.sendInputEvent({type:'mouseDown',button:'left',clickCount:1,...link});
        guest.sendInputEvent({type:'mouseUp',button:'left',clickCount:1,...link});
      });
    },{dst,port:contentPort});
    assert.equal(receipt.state,'completed');assert.deepEqual(await readFile(dst),attachment);
    console.log('PASS: trusted guest click requests native Save options, exact bytes in an isolated destination');
  }
  await assert.rejects(() => page.evaluate(target => window.luvus.request({ type: 'action', id: 'forged-lease',
    generation: target.target.generation, method: 'browser.control.acquire', params: { browser: target.browser_id } }), target), /native client/);
  if (process.argv.includes('--cdp')) {
    const { observeTransfers } = await import('./cdp-transfers.mjs');
    await observeTransfers({ binary, browserId: target.browser_id, home, workspace, env: ownerEnv, page, attachment });
    if (process.env.LUVUS_TEST_AGENT_BROWSER) {
      const {observeAgentBrowser}=await import('./agent-browser.mjs');
      await observeAgentBrowser({tool:process.env.LUVUS_TEST_AGENT_BROWSER,binary,browserId:target.browser_id,env:ownerEnv,workspace,home});
    }
  }
  await assert.rejects(() => browser('evaluate', ...control, '--expression', '1'));
  const events = await page.evaluate(() => window.__browserEvents);
  assert.ok(!JSON.stringify(events).includes(acquired.lease));
  assert.ok(!JSON.stringify(events).includes('fixture-pass'));
  await assert.rejects(() => page.evaluate(target => window.luvus.request({ type: 'browser_result', id: 'forged', target, lease_generation: '0', value: {}, error: null }), target.target));
  if(!remote){const {observeLocalNetwork}=await import('./browser-network.mjs');await observeLocalNetwork(desktop,home);}
  console.log(`PASS: embedded owner proxy, CLI snapshot/fill/click/evaluate/screenshot, takeover and renderer isolation; artifacts ${home}`);
} catch (error) {
  if (page) {
    await page.screenshot({ path: join(home, 'failure.png') }).catch(() => {});
    console.error((await page.locator('body').innerText().catch(() => 'No renderer text')).replace(/\n{3,}/g, '\n\n'));
    console.error(await page.evaluate(async () => (await window.luvus.bootstrap()).filter(message => message.type === 'error' || message.type === 'ready')).catch(() => []));
  }
  console.error(`Browser fixture artifacts: ${home}`);
  throw error;
} finally {
  await desktop?.close();
  if (started) {
    assert.equal(await readFile(join(home, '.isolated-app'), 'utf8'), home);
    await command('server', 'stop');
  }
  await Promise.all([close(ownerProxy), close(displayProxy), close(content)]);
}
