// Real registered-host path: Electron attaches locally. Only the fixture's
// pooled display child is interrupted; the owner, PTYs and network stay up.
import { useEnglishUi } from './device-language.mjs';
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { mkdir, mkdtemp, readFile, realpath, symlink, unlink, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { setTimeout as sleep } from 'node:timers/promises';
import { _electron } from 'playwright-core';
import { isolatedEnvironment } from '../scripts/dev.mjs';

const [baseHost, remote, mode] = process.argv.slice(2);
const jump = mode==='--jump', host=jump?'luvus-fixture-owner':baseHost;
if (mode && !jump || !/^[A-Za-z0-9_.][\w.-]*$/.test(baseHost ?? '') || !/^\/[\w./-]+\/target\/app-e2e-real-[A-Za-z0-9]+$/.test(remote ?? '')
    || remote.split('/').includes('..') || process.env.LUVUS_ENV) throw new Error('Expected SSH alias and marked fixture, outside Luvus');
const root = await realpath(resolve(dirname(fileURLToPath(import.meta.url)), '../..'));
const bundle=process.env.LUVUS_TEST_BUNDLE ? await realpath(process.env.LUVUS_TEST_BUNDLE) : null;
const binary = bundle?join(bundle,'Contents/Resources/luvus'):join(root, 'target/debug/luvus');
assert.equal(await realpath(binary), binary);
const local = await mkdtemp(join(root, 'target/app-e2e-ssh-'));
const marker = 'REG_' + local.slice(-6) + '_';
console.log('Registered SSH fixture: ' + local);
for (const name of ['bin', 'electron', 'tmp', 'state', '.ssh']) await mkdir(join(local, name), { mode: 0o700 });
const options = ['-o', 'BatchMode=yes', '-o', 'StrictHostKeyChecking=yes', '-o', 'ForwardAgent=no',
  '-o', 'ControlMaster=no', '-o', 'ControlPath=none', '-o', 'ConnectTimeout=8', '-o', 'PermitLocalCommand=no'];
const exec = promisify(execFile);
const ssh = '/usr/bin/ssh';
const { stdout: config } = await exec(ssh, [...options, '-G', baseHost]);
assert.doesNotMatch(config, /^(remotecommand|localforward|remoteforward|dynamicforward) /m);
const confPath=join(local,'.ssh/config');
if (jump) {
  // Two authenticated SSH hops on the authorized machine. Do not reuse the
  // user's master or forward the agent. The final 127.0.0.1 is hop-relative.
  assert.doesNotMatch(config,/^(proxycommand|proxyjump) /m);
  const field = key=>config.match(new RegExp('^'+key+' (.+)$','m'))?.[1];
  const hostname=field('hostname'),user=field('user'),port=field('port'),key=field('hostkeyalias')??hostname;
  for (const word of [hostname,user,port,key]) assert.match(word??'',/^[\w.:-]+$/);
  const identities=[...config.matchAll(/^identityfile (.+)$/gm)].map(m=>m[1]);
  for (const path of identities) assert.match(path,/^[\w./~ -]+$/);
  const common='  User '+user+'\n  Port '+port+'\n  HostKeyAlias '+key+'\n  StrictHostKeyChecking yes\n  BatchMode yes\n  ForwardAgent no\n  ControlMaster no\n  ControlPath none\n'+identities.map(p=>'  IdentityFile "'+p+'"\n').join('');
  await writeFile(confPath,'Host luvus-fixture-hop\n  HostName '+hostname+'\n'+common+'Host '+host+'\n  HostName 127.0.0.1\n  ProxyJump luvus-fixture-hop\n'+common,{mode:0o600});
  options.push('-F',confPath);
  const route=await exec(ssh,[...options,'-G',host]);
  assert.match(route.stdout,/^proxyjump luvus-fixture-hop$/m);
} else {
  await writeFile(confPath, 'Host ' + host + '\n');
}
await writeFile(join(local, '.isolated-ssh'), JSON.stringify({ host, root: remote, options, registered: true }), { flag: 'wx', mode: 0o600 });
await symlink(join(root, 'app/tests/real-owner.py'), join(local, 'bin/ssh'));
await writeFile(join(local, 'state/config.json'), JSON.stringify({ check_updates: false, shell: '/bin/sh', remote_hosts: [] }));
const env = { ...isolatedEnvironment(), HOME: local, TMPDIR: join(local, 'tmp'), LUVUS_HOME: join(local, 'state'),
  XDG_DATA_HOME: join(local, 'data'), XDG_CONFIG_HOME: join(local, 'config'), XDG_CACHE_HOME: join(local, 'cache'),
  LUVUS_APP_USER_DATA: join(local, 'electron'), LUVUS_APP_BINARY: binary, APP_SSH_LOCAL: local,
  PATH: join(local, 'bin') + ':' + process.env.PATH };
const cli = async (...args) => (await exec(binary, ['--session', 'ssh', ...args], { cwd: local, env, timeout: 25_000 })).stdout;
const owner = async (...args) => {
  const command = ['python3', remote + '/real-owner.py', 'cli', ...args].map(s => "'" + s.replaceAll("'", "'\\''") + "'").join(' ');
  return (await exec(ssh, [...options, '-T', '-o', 'ClearAllForwardings=yes', host, command], { timeout: 25_000 })).stdout;
};
const paths = [remote + '/workspace', remote + '/home'];
const until = async (label, fn) => {
  const end = Date.now() + 35_000;
  do { const value = await fn(); if (value) return value; await sleep(80); } while (Date.now() < end);
  throw new Error('Timeout: ' + label);
};
const routes = async () => (await readFile(join(local, 'ssh-routes.jsonl'), 'utf8')).trim().split('\n').map(JSON.parse);
const displays = async () => (await routes()).filter(args => args.at(-1)?.includes('remote-client-bridge'));
let desktop, page, startedOwner = false, startedLocal = false, seq = 0;
try {
  const version = (await owner('--version')).trim();
  assert.match(version, /build-id=app-e2e-real-/);
  assert.match(version, /transport=21\b/);
  assert.match(await owner('server', 'status'), /not running/, 'refusing to share an existing fixture owner');
  await owner('server', 'start'); startedOwner = true;
  for (const path of paths) {
    await owner('workspace', 'open', path);
    // The direct-browser fixture may have restored a focused native browser.
    // Use a fresh, explicit terminal in each marked workspace for routing proof.
    const snapshot=JSON.parse(await owner('uhp','snapshot')).result;
    assert.equal(snapshot.workspaces.find(w=>w.active)?.cwd,path);
    await owner('tab','new');
  }
  await cli('server', 'start'); startedLocal = true;
  if(bundle)delete env.LUVUS_APP_BINARY;
  desktop = await _electron.launch({ ...(bundle?{executablePath:join(bundle,'Contents/MacOS/Luvus')}:{}),args: [...(bundle?[]:[join(root,'app/apps/desktop/dist/main.cjs')]), '--session', 'ssh'], cwd: local, env, timeout: 30_000 });
  page = await desktop.firstWindow();
  await useEnglishUi(page);
  await page.locator('.session-selector').click();
  await page.getByRole('button',{name:'Manage connections',exact:true}).click();
  await page.getByRole('button',{name:'List sessions'}).click();
  await page.getByRole('button',{name:'Add host…'}).click();
  await page.getByRole('dialog').getByRole('textbox').fill(host);
  await page.getByRole('button',{name:'Connect host'}).click();
  await page.getByRole('dialog').waitFor({state:'hidden',timeout:35_000});
  const merge = page.getByRole('checkbox',{name:'Merge same-name sessions'});
  assert.ok(!await merge.isChecked());
  await merge.click();
  await page.getByRole('button',{name:'Enable merge'}).click();
  await page.getByRole('dialog').waitFor({state:'hidden',timeout:35_000});
  assert.ok(await merge.isChecked());
  await page.getByRole('button',{name:'Back to terminals'}).click();
  await page.getByRole('button',{name:'Open workspace',exact:true}).first().click();
  const picker = page.getByRole('dialog');
  await picker.getByRole('combobox',{name:'Workspace session'}).click();
  await page.getByRole('option',{name:host+' / ssh',exact:true}).click();
  assert.equal(await picker.getByRole('button',{name:'Browse'}).count(),0);
  await picker.getByRole('textbox',{name:'Directory'}).fill(remote+'/state');
  await picker.getByRole('button',{name:'Open workspace',exact:true}).click();
  await picker.waitFor({state:'hidden',timeout:35_000});
  await until('directory opened on remote owner',async()=>JSON.parse(await owner('uhp','snapshot')).result.workspaces.some(w=>w.cwd===remote+'/state'));
  const snap = () => page.evaluate(async () => (await window.luvus.bootstrap()).find(r => r.type === 'snapshot')?.snapshot);
  const select = async i => {
    await page.locator('.workspace-main').and(page.locator('[title="' + paths[i] + '"]')).click({ timeout: 30_000 });
    return until('committed ' + paths[i], async () => {
      const ws = (await snap())?.workspaces.find(w => w.active && w.cwd === paths[i] && w.host === host && w.gui_available);
      return ws?.tabs.find(t => t.active)?.panes.find(p => p.focused && p.target?.terminal_id)?.target;
    });
  };
  const input = (target, value) => page.evaluate(request => window.luvus.request(request), { type: 'input', id: 'e2e-' + ++seq, target, input: value });
  const rejected = async target => {
    await assert.rejects(input(target, { kind: 'paste', text: 'STALE_SHOULD_NEVER_ARRIVE' }), /stale_target|registered workspace selection changed/);
  };
  const run = async (target, label) => {
    // Split the marker so shell echo alone cannot satisfy the rendered-output check.
    assert.equal((await input(target, { kind: 'paste', text: "printf '%s%s\\n' '" + marker + "' '" + label + "'" })).accepted, true);
    assert.equal((await input(target, { kind: 'key', key: 'Enter' })).accepted, true);
    await page.waitForFunction(marker => document.querySelector('.terminal-accessible')?.textContent.includes(marker),
      marker + label, { timeout: 20_000 });
  };
  const a1 = await select(0);
  await run(a1, 'FIRST_A');
  // Construct A's input before switching; release it only after B is painted.
  const delayed = { type: 'input', id: 'delayed-A', target: a1, input: { kind: 'paste', text: 'STALE_SHOULD_NEVER_ARRIVE' } };
  const b = await select(1);
  await assert.rejects(page.evaluate(req => window.luvus.request(req), delayed), /stale_target|registered workspace selection changed/);
  await rejected(a1);
  await run(b, 'ONLY_B');
  await page.locator('canvas').first().click();
  await page.keyboard.type("printf '%s%s\\n' '" + marker + "' 'KEYBOARD_B'");
  await page.keyboard.press('Enter');
  await page.waitForFunction(s => document.querySelector('.terminal-accessible')?.textContent.includes(s),
    marker + 'KEYBOARD_B', { timeout: 20_000 });
  const a2 = await select(0);
  assert.notEqual(a1.pane_id, a2.pane_id);
  assert.equal(a1.terminal_id, a2.terminal_id);
  await rejected(a1); await rejected(b);
  await run(a2, 'BEFORE_LOSS');
  assert.equal((await displays()).length, 1, 'A/B switches opened a second SSH display');
  const since = performance.now();
  await writeFile(join(local, 'hold-display'), 'disconnect only this display\n', { flag: 'wx' });
  const status = await until('retired disconnected view', async () => {
    const ws = (await snap())?.workspaces.find(w => w.active && w.cwd === paths[0]);
    return ws && !ws.gui_available && ws.projection_status === 'disconnected' && ws.projection_status;
  });
  await until('failed reconnect with the display still unavailable', async () => (await displays()).length >= 2);
  await rejected(a2);
  await unlink(join(local, 'hold-display'));
  const a3 = await until('automatically restored A', async () => {
    const ws = (await snap())?.workspaces.find(w => w.active && w.cwd === paths[0] && w.gui_available);
    return ws?.tabs.find(t => t.active)?.panes.find(p => p.focused)?.target;
  });
  const recoveryMs = Math.round(performance.now() - since);
  assert.equal(a2.terminal_id, a3.terminal_id); assert.notEqual(a2.pane_id, a3.pane_id);
  await rejected(a2); await rejected(b);
  await run(a3, 'RECOVERED_A');
  const b2 = await select(1);
  await rejected(a3); await rejected(b);
  await run(b2, 'RECOVERED_B');
  const pane = t => t.pane_id.split(':').at(-1);
  const readA = await owner('pane', 'read', pane(a3)), readB = await owner('pane', 'read', pane(b2));
  for (const [here, there, label] of [[readA, readB, 'FIRST_A'], [readA, readB, 'RECOVERED_A'],
    [readB, readA, 'ONLY_B'], [readB, readA, 'KEYBOARD_B'], [readB, readA, 'RECOVERED_B']]) {
    assert.ok(here.includes(marker + label)); assert.ok(!there.includes(marker + label));
  }
  assert.ok(!readA.includes('STALE_SHOULD_NEVER_ARRIVE') && !readB.includes('STALE_SHOULD_NEVER_ARRIVE'));
  // Stop only the marked owner created above. Discovery and retry are not
  // authorized to restart it. Then explicitly start it to test the new boot.
  await owner('server','stop'); startedOwner=false;
  await until('previous owner generation retired',async()=>{
    const ws=(await snap())?.workspaces.find(w=>w.active && w.cwd===paths[1]);
    return !ws || !ws.gui_available;
  });
  await rejected(b2);
  assert.match(await owner('server','status'),/not running/);
  await owner('server','start'); startedOwner=true;
  const restarted=await select(1);
  assert.notEqual(restarted.pane_id,b2.pane_id);
  assert.notEqual(restarted.terminal_id,b2.terminal_id);
  await rejected(b2); await run(restarted,'NEW_OWNER_B');
  const afterRestart=await owner('pane','read',pane(restarted));
  assert.ok(afterRestart.includes(marker+'NEW_OWNER_B') && !afterRestart.includes('STALE_SHOULD_NEVER_ARRIVE'));
  const png = await desktop.evaluate(async ({ BrowserWindow }) => (await BrowserWindow.getAllWindows()[0].capturePage()).toPNG().toString('base64'));
  await writeFile(join(local, 'desktop.png'), Buffer.from(png, 'base64'));
  await owner('browser','panel','http://registered-fixture.invalid/','--pane',pane(restarted));
  await page.getByText(/unsupported projected view/i).first().waitFor();
  assert.equal(await page.getByRole('button',{name:'Open embedded browser',exact:true}).count(),0);
  assert.ok(!(await routes()).some(args=>args.includes('-D')),'unavailable registered browser must not open a tunnel');
  await page.screenshot({path:join(local,'registered-browser-unavailable.png')});
  await desktop.close(); desktop = undefined;
  const report = { binary,host, baseHost, proxyJump:jump, version, status, recoveryMs, a1, b, a2, a3, b2, readA, readB, displayAttempts: (await displays()).length,
    graphicalHostAdmission:true, graphicalSameNameMerge:true, registeredBrowserUnavailable:true, restarted, afterRestart, delayedInputRejected: true, staleMarkerOnEitherOwnerPane: false,
    ownerAfterDetach: (await owner('server', 'status')).trim() };
  await writeFile(join(local, 'result.json'), JSON.stringify(report, null, 2));
} catch (error) {
  if (page && !page.isClosed()) console.error((await page.locator('body').innerText()).slice(0, 2000));
  throw error;
} finally {
  try { await desktop?.close(); } finally {
    try {
      if (startedLocal) { await cli('server', 'stop'); assert.match(await cli('server', 'status'), /not running/); }
    } finally {
      if (startedOwner) { await owner('server', 'stop'); assert.match(await owner('server', 'status'), /not running/); }
    }
  }
}
console.log('PASS: registered SSH switch, stale input fencing, recovery, unchanged PTYs; ' + local);
