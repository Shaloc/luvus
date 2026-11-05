// Opt in with an SSH alias and a pre-provisioned isolated fixture. Never use
// the PATH-installed remote binary or the host's default Luvus namespace.
import assert from 'node:assert/strict';
import { execFile, spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdtemp, mkdir, readdir, readFile, realpath, symlink, unlink, writeFile } from 'node:fs/promises';
import {createSocket} from 'node:dgram';
import { createServer } from 'node:http';
import { dirname, join, resolve } from 'node:path';
import { createInterface } from 'node:readline';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { _electron } from 'playwright-core';
import { isolatedEnvironment } from '../scripts/dev.mjs';

const [host, remote] = process.argv.slice(2);
if (!/^[A-Za-z0-9_.][\w.-]*$/.test(host ?? '') || !/^\/[\w./-]+\/target\/app-e2e-real-[A-Za-z0-9]+$/.test(remote ?? '')
    || remote.split('/').includes('..') || process.env.LUVUS_ENV) throw new Error('Expected SSH alias and marked fixture path, outside Luvus');
const root = await realpath(resolve(dirname(fileURLToPath(import.meta.url)), '../..'));
const bundle=process.env.LUVUS_TEST_BUNDLE ? await realpath(process.env.LUVUS_TEST_BUNDLE) : null;
const binary = bundle?join(bundle,'Contents/Resources/luvus'):join(root, 'target/debug/luvus');
assert.equal(await realpath(binary), binary);
const local = await mkdtemp(join(root, 'target/app-e2e-ssh-'));
for (const name of ['bin', 'electron', 'tmp', 'state']) await mkdir(join(local, name), { mode: 0o700 });
const options = ['-o', 'BatchMode=yes', '-o', 'StrictHostKeyChecking=yes', '-o', 'ForwardAgent=no',
  '-o', 'ControlMaster=no', '-o', 'ControlPath=none', '-o', 'ConnectTimeout=8', '-o', 'PermitLocalCommand=no'];
const exec = promisify(execFile);
const ssh = '/usr/bin/ssh';
const { stdout: effective } = await exec(ssh, [...options, '-G', host]);
if (/^(remotecommand|localforward|remoteforward|dynamicforward) /m.test(effective)) throw new Error('Unexpected configured SSH command/forward');
await writeFile(join(local, '.isolated-ssh'), JSON.stringify({ host, root: remote, options }), { flag: 'wx', mode: 0o600 });
await symlink(join(root, 'app/tests/real-owner.py'), join(local, 'bin/ssh'));
const quote = word => "'" + word.replaceAll("'", "'\\''") + "'";
const command = args => ['python3', remote + '/real-owner.py', ...args].map(quote).join(' ');
const sshArgs = args => [...options, '-T', '-o', 'ClearAllForwardings=yes', host, command(args)];
const run = async (...args) => (await exec(ssh, sshArgs(args), { timeout: 60_000, maxBuffer: 2 * 1024 * 1024 })).stdout;
const cli = (...args) => run('cli', ...args);
const api = async (...args) => {
  const result = JSON.parse(await cli(...args));
  assert.ok(!result.error, 'owner rejected the browser request');
  return result.result;
};
const service = spawn(ssh, sshArgs(['serve']), { stdio: ['pipe', 'pipe', 'inherit'] });
const serviceExit = once(service, 'exit');
const lines = createInterface({ input: service.stdout });
let requests = 0, started = false, desktop;
const trap = createServer((_req, res) => { requests++; res.end('DISPLAY LOCALHOST'); });
try {
  const [line] = await Promise.race([once(lines, 'line', { signal: AbortSignal.timeout(12_000) }),
    serviceExit.then(() => { throw new Error('Owner HTTP fixture failed'); })]);
  const ports = JSON.parse(line);
  assert.ok(Number.isInteger(ports.origin) && ports.origin > 1024);
  const listening = once(trap, 'listening');
  trap.listen(ports.origin, '127.0.0.1');
  await listening;
  const version = await cli('--version');
  assert.match(version, /transport=21\b/);
  assert.match(version, /build-id=app-e2e-real-/);
  assert.match(await cli('server','status'),/not running/,'refusing to share a live fixture owner');
  await cli('server', 'start');
  started = true;
  await cli('workspace', 'open', remote + '/workspace');
  const env = { ...isolatedEnvironment(), HOME: local, TMPDIR: join(local, 'tmp'), LUVUS_HOME: join(local, 'state'),
    XDG_DATA_HOME: join(local, 'data'), XDG_CONFIG_HOME: join(local, 'config'), XDG_CACHE_HOME: join(local, 'cache'),
    LUVUS_APP_USER_DATA: join(local, 'electron'), LUVUS_APP_BINARY: binary, APP_SSH_LOCAL: local,
    PATH: join(local, 'bin') + ':' + process.env.PATH };
  if(bundle)delete env.LUVUS_APP_BINARY;
  desktop = await _electron.launch({...(bundle?{executablePath:join(bundle,'Contents/MacOS/Luvus')}:{}),args: [...(bundle?[]:[join(root,'app/apps/desktop/dist/main.cjs')]), '--remote', host, '--session', 'ssh'],
    cwd: local, env, timeout: 30_000 });
  const page = await desktop.firstWindow();
  await page.locator('canvas').first().waitFor({ state: 'visible', timeout: 20_000 });
  await page.getByRole('button', { name: `ssh SSH: ${host}` }).waitFor();
  await page.locator('canvas').first().click();
  const inputStarted = performance.now();
  await page.keyboard.type("printf 'SSH owner: %s %s\\n' \"$(uname -s)\" \"$(uname -m)\"");
  await page.keyboard.press('Enter');
  const painted = () => page.waitForFunction(() => document.querySelector('.terminal-accessible')?.textContent.includes('SSH owner: Linux'), null, { timeout: 25_000 })
    .catch(async error => {
      const diagnostic = await page.evaluate(async () => ({
        painted: document.querySelector('.terminal-accessible')?.textContent,
        frames: (await window.luvus.bootstrap()).filter(m => m.type === 'terminal').map(m => ({
          revision: m.frame.revision, w: m.frame.cols, h: m.frame.rows, text: m.frame.cells.map(c => c.text).join('').trim(),
        })),
      }));
      throw new Error('Visible SSH terminal is stale: ' + JSON.stringify({ ...diagnostic,
        owner: await cli('pane', 'read'), active: await page.evaluate(() => document.activeElement?.getAttribute('aria-label')),
        shell: await page.locator('body').innerText() }), { cause: error });
    });
  await painted();
  const inputMs = Math.round(performance.now() - inputStarted);
  await page.getByRole('button', { name: 'Open browser leaf', exact: true }).click();
  await page.getByRole('textbox', { name: /HTTP\(S\) URL/ }).fill('http://owner-only.invalid/');
  await page.getByRole('button', { name: 'Open browser', exact: true }).click();
  await page.getByRole('button', { name: 'Open embedded browser', exact: true }).click();
  await page.waitForFunction(() => document.querySelector('[aria-label="Browser address"]')?.disabled === false, null, { timeout: 30_000 });
  const target = (await api('browser', 'list')).browsers.find(b => b.state === 'ready');
  assert.ok(target);
  const { lease } = await api('browser', 'control', 'acquire', '--browser', target.browser_id, '--ttl', '60');
  const authority = ['--browser', target.browser_id, '--lease', lease];
  assert.equal((await api('browser', 'snapshot', ...authority)).data.title, 'OWNER PROXY');
  await api('browser', 'navigate', ...authority, '--url', `http://localhost:${ports.origin}/`);
  await api('browser', 'wait', ...authority, '--condition', '{"load":true}');
  assert.equal((await api('browser', 'snapshot', ...authority)).data.title, 'OWNER LOCALHOST');
  const network={};
  const guest=await desktop.evaluate(({webContents},port)=>webContents.getAllWebContents().find(c=>c.getURL()==='http://localhost:'+port+'/')?.id,ports.origin);
  assert.ok(guest);
  const netfile=join(local,'guest-probe.netlog');
  const udp=createSocket('udp4');let datagrams=0;udp.on('message',()=>datagrams++);
  await new Promise(r=>udp.bind(0,'127.0.0.1',r));
  try{
    await desktop.evaluate(async({webContents},{guest,netfile})=>webContents.fromId(guest).session.netLog.startLogging(netfile),{guest,netfile});
    network.ws=await desktop.evaluate(({webContents},id)=>webContents.fromId(id).executeJavaScript(`new Promise((res,rej)=>{const ws=new WebSocket('ws://'+location.host+'/socket');const t=setTimeout(()=>{ws.close();rej(Error('websocket timed out'))},6000);ws.onmessage=e=>{clearTimeout(t);res(e.data);ws.close()};ws.onerror=()=>{clearTimeout(t);rej(Error('socket failed'))}})`),guest);
    assert.equal(network.ws,'OWNER_WS');
    network.ice=await desktop.evaluate(({webContents},{guest,port})=>webContents.fromId(guest).executeJavaScript(`(async()=>{
      const pc=new RTCPeerConnection({iceServers:[{urls:'stun:127.0.0.1:${port}'}]});let candidates=0;
      const done=new Promise(r=>{pc.onicecandidate=e=>{if(e.candidate)candidates++;else r()};setTimeout(r,2500)});
      try{pc.createDataChannel('probe');await pc.setLocalDescription(await pc.createOffer());await done;return {candidates,gathering:pc.iceGatheringState};}finally{pc.close();}
    })()`),{guest,port:udp.address().port});
    assert.equal(network.ice.candidates,0);assert.equal(datagrams,0);network.displayStunPackets=datagrams;
    await desktop.evaluate(({webContents},id)=>webContents.fromId(id).executeJavaScript("fetch('http://dns-probe.luvus.invalid/probe',{mode:'no-cors',signal:AbortSignal.timeout(3000)}).catch(()=>null).then(()=>true)"),guest);
    await desktop.evaluate(async({webContents},id)=>webContents.fromId(id).session.netLog.stopLogging(),guest);
    const capture=JSON.parse(await readFile(netfile,'utf8'));
    const types=capture.constants.logEventTypes;
    assert.ok(capture.events.some(e=>e.type===types.URL_REQUEST_START_JOB && JSON.stringify(e.params??{}).includes('dns-probe.luvus.invalid')));
    const forbidden=new Set(['DNS_TRANSACTION','HOST_RESOLVER_SYSTEM_TASK','QUIC_SESSION'].map(k=>types[k]).filter(Number.isInteger));
    const leaked=capture.events.filter(e=>forbidden.has(e.type) && JSON.stringify(e.params??{}).includes('dns-probe.luvus.invalid'));
    assert.equal(leaked.length,0);network.displayResolverEventsForProbe=leaked.length;
    await unlink(netfile);
  }finally{udp.close();}
  if(ports.ipv6){
    await api('browser','navigate',...authority,'--url','http://[::1]:'+ports.ipv6+'/');
    await api('browser','wait',...authority,'--condition','{"text":"OWNER LOCALHOST"}');
    network.ipv6=(await api('browser','snapshot',...authority)).data.title;
    assert.equal(network.ipv6,'OWNER LOCALHOST');
  }else{
    network.ipv6='unsupported on owner';
  }
  await api('browser','navigate',...authority,'--url',`http://localhost:${ports.origin}/`);
  await api('browser','wait',...authority,'--condition','{"load":true}');
  const capture = await api('browser', 'screenshot', ...authority, '--output', 'owner.png');
  assert.equal(capture.data.path, remote + '/workspace/owner.png');
  await api('browser', 'control', 'release', ...authority);
  const transfer = JSON.parse(await run('cdp', target.browser_id));
  await painted();
  assert.equal(transfer.ownerDownload, remote + '/workspace/remote.bin');
  assert.equal(requests, 0, 'guest accessed the display at the same localhost port');
  const screenshot = await desktop.evaluate(async ({ BrowserWindow }) => (await BrowserWindow.getAllWindows()[0].capturePage()).toPNG().toString('base64'));
  await writeFile(join(local, 'desktop.png'), Buffer.from(screenshot, 'base64'));
  const windowSource = await desktop.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0].getMediaSourceId());
  await exec('/usr/sbin/screencapture', ['-x', '-l', windowSource.split(':')[1], join(local, 'desktop-native.png')], { timeout: 10_000 });
  const ack=once(lines,'line',{signal:AbortSignal.timeout(8000)});service.stdin.write('p');
  assert.equal((await ack)[0],'proxy-stopped');
  // The very same numeric destination is live on both machines. With the
  // upstream proxy stopped, neither display-direct nor owner-direct may win.
  const body=await desktop.evaluate(async({webContents},{guest,port})=>{
    const w=webContents.fromId(guest);await w.loadURL('http://127.0.0.1:'+port+'/must-use-proxy');
    return w.executeJavaScript('document.body.innerText');
  },{guest,port:ports.origin});
  assert.match(body,/owner browser proxy connection failed/i);assert.equal(requests,0);
  network.proxyInterruptedWithoutFallback=true;
  await desktop.close(); desktop = undefined;
  assert.deepEqual(await readdir(join(local, 'state')), [], 'direct attach created local owner state');
  const live = await cli('server', 'status');
  const routes = (await readFile(join(local, 'ssh-routes.jsonl'), 'utf8')).trim().split('\n').map(JSON.parse);
  assert.ok(routes.some(args => args.includes('-D')) && routes.every(args => !args.includes('-R')));
  const report = { binary,host, version: version.trim(), generation: target.target.generation, ports, network, displayRequests: requests, inputMs,
    screenshot: capture.data, transfer, serverAfterDetach: live.trim() };
  await writeFile(join(local, 'result.json'), JSON.stringify(report, null, 2));
  console.log('PASS: real SSH direct App, owner HTTP proxy, NO_PROXY localhost, owner file transfers and detach; ' + local);
} finally {
  try { await desktop?.close(); }
  finally {
    try { if (started) await cli('server', 'stop'); }
    finally {
      lines.close(); service.stdin.end(); await serviceExit;
      if (trap.listening) await new Promise(finish => trap.close(finish));
      console.log('SSH fixture: ' + local);
    }
  }
}
