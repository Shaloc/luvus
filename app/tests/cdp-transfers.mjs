import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdir, readdir, readFile, stat, symlink, truncate, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { createInterface } from 'node:readline';
import { setTimeout as delay } from 'node:timers/promises';
import { chromium } from 'playwright-core';

export async function observeTransfers({ binary, browserId, home, workspace, env, page, attachment }) {
  const gateway = spawn(binary, ['--session', env.LUVUS_SESSION, 'browser', 'cdp', '--browser', browserId,
    '--ttl', '120', '--json'], { cwd: workspace, env, stdio: ['ignore', 'pipe', 'inherit'] });
  const lines = createInterface({ input: gateway.stdout });
  const exited = once(gateway, 'exit');
  let connection;
  try {
    const [line] = await Promise.race([once(lines, 'line', { signal: AbortSignal.timeout(30_000) }),
      exited.then(() => { throw new Error('Gateway exited before printing a descriptor'); })]);
    connection = await chromium.connectOverCDP(JSON.parse(line).endpoint, { noDefaults: true, timeout: 15_000 });
    const context = connection.contexts()[0];
    assert.equal(context.pages().length, 1);
    const guest = context.pages()[0];
    const cdp = await context.newCDPSession(guest);
    const crossSite=new URL(guest.url()); crossSite.hostname='matrix-child.invalid'; crossSite.pathname='/child';
    await guest.evaluate(url => {
      const frame = document.createElement('iframe');
      frame.id = 'cross-site-child';
      // Exercise the native input fence with a fractional cross-process frame origin.
      frame.style.cssText = 'position:absolute;left:120.5px;top:80.5px;width:350px;height:220px';
      frame.src = url;
      document.body.append(frame);
    }, crossSite.href);
    const child=guest.frameLocator('#cross-site-child');
    await child.getByRole('textbox',{name:'Name'}).fill('child scope',{timeout:12_000});
    await child.getByRole('button',{name:'Greet'}).click({timeout:12_000});
    await child.getByText('Hello child scope',{exact:true}).waitFor({timeout:12_000});
    assert.deepEqual(await child.locator('body').evaluate(()=>[typeof process,typeof require,typeof window.luvus]),['undefined','undefined','undefined']);
    assert.equal(context.pages().length,1);
    await guest.evaluate(()=>document.querySelector('#cross-site-child').remove());
    console.log('PASS: Playwright controls an authorized cross-site child; there is no Node or desktop bridge in the frame');
    const { root } = await cdp.send('DOM.getDocument');
    const { nodeId } = await cdp.send('DOM.querySelector', { nodeId: root.nodeId, selector: '#upload' });
    const select = files => cdp.send('DOM.setFileInputFiles', { nodeId, files });
    const payload = Buffer.from(Array.from({ length: 24_321 }, (_, i) => i % 256));
    const owned = join(workspace, 'payload.bin');
    const outside = join(home, 'outside.bin');
    await writeFile(owned, payload);
    await writeFile(join(workspace, 'empty.txt'), '');
    await writeFile(outside, 'not authorized');
    await symlink(outside, join(workspace, 'escape.bin'));
    await select([owned, 'empty.txt']);
    const input = guest.locator('#upload');
    const files = await input.evaluate(async input => Promise.all(Array.from(input.files, async file => ({
      name: file.name, bytes: Array.from(new Uint8Array(await file.arrayBuffer())),
    }))));
    assert.deepEqual(files.map(file => file.name), ['payload.bin', 'empty.txt']);
    assert.deepEqual(Buffer.from(files[0].bytes), payload);
    assert.deepEqual(files[1].bytes, []);
    await guest.screenshot({ path: join(home, 'owner-upload.png') });
    for (const path of [outside, '../outside.bin', 'escape.bin']) await assert.rejects(select([path]), /file_denied/);
    const oversized = join(workspace, 'oversized.bin');
    await writeFile(oversized, '');
    await truncate(oversized, 64 * 1024 * 1024 + 1);
    await assert.rejects(select([oversized]), /limit_exceeded/);
    assert.equal(await input.evaluate(input => input.files.length), 2);
    await select([]);
    assert.equal(await input.evaluate(input => input.files.length), 0);
    const browser = await connection.newBrowserCDPSession();
    await assert.rejects(browser.send('Browser.close'));
    await assert.rejects(browser.send('Target.attachToTarget',{targetId:'not-this-page',flatten:true}));
    const destination = join(workspace, 'artifacts');
    await mkdir(destination);
    const stages=async()=>Promise.all((await readdir(env.TMPDIR)).filter(n=>n.startsWith('luvus-browser-download-')).map(async name=>({name,inode:(await stat(join(env.TMPDIR,name)).catch(()=>({ino:null}))).ino})));
    const trace=[];
    const configure = async(behavior,downloadPath)=>{
      const result=await browser.send('Browser.setDownloadBehavior',{behavior,downloadPath,eventsEnabled:true});
      trace.push({behavior,stages:await stages()});
      return result;
    };
    await assert.rejects(configure('allow', home), /file_denied/);
    await assert.rejects(configure('allow', join(workspace, 'outside')), /file_denied/);
    const download = async () => {
      const signal = AbortSignal.timeout(10_000);
      const started = once(browser, 'Browser.downloadWillBegin', { signal });
      const finished = once(browser, 'Browser.downloadProgress', { signal });
      const action = guest.evaluate(() => {
        const link = document.createElement('a'); link.href = '/attachment'; link.download = 'artifact.bin'; link.click();
      });
      const [[begin], [end]] = await Promise.all([started, finished, action]);
      assert.equal(begin.guid, end.guid);
      return end;
    };
    await configure('allowAndName', destination);
    const byGuid = await download();
    assert.equal(byGuid.state, 'completed');
    assert.equal(byGuid.filePath, join(destination, byGuid.guid));
    assert.deepEqual(await readFile(byGuid.filePath), attachment);
    await configure('allow', destination);
    const byName = await download();
    assert.equal(byName.filePath, join(destination, 'artifact.bin'));
    assert.deepEqual(await readFile(byName.filePath), attachment);
    const collision = await download();
    assert.equal(collision.state, 'canceled');
    assert.equal(collision.filePath, undefined);
    assert.deepEqual(await readFile(byName.filePath), attachment);
    const staged = () => readdir(env.TMPDIR).then(names => names.filter(name => name.startsWith('luvus-browser-upload-')));
    assert.equal((await staged()).length, 2);
    const startSlow = async () => {
      const started = once(browser, 'Browser.downloadWillBegin', { signal: AbortSignal.timeout(10_000) });
      await guest.evaluate(() => {
        const link = document.createElement('a'); link.href = '/slow-download'; link.download = 'incomplete.bin'; link.click();
      });
      await started;
    };
    await startSlow();
    const revoked = once(browser, 'Browser.downloadProgress', { signal: AbortSignal.timeout(10_000) });
    await configure('deny');
    assert.equal((await revoked)[0].state, 'canceled');
    assert.deepEqual((await readdir(destination)).sort(), ['artifact.bin', byGuid.guid].sort());
    await configure('allow', destination);
    await startSlow();
    await page.getByRole('button', { name: 'Take control', exact: true }).click();
    assert.deepEqual(await exited, [0, null]);
    const pendingTransfers=async()=>(await readdir(env.TMPDIR)).filter(name=>/^luvus-browser-(upload|download)-/.test(name));
    for (let i = 0; i < 100 && (await pendingTransfers()).length > 0; i++) await delay(20);
    assert.deepEqual(await staged(), []);
    const remaining=await stages();await writeFile(join(home,'transfer-cleanup.json'),JSON.stringify({trace,remaining}));
    assert.deepEqual(remaining,[]);
    assert.equal((await readdir(destination)).length, 2);
    assert.deepEqual(await readFile(owned), payload);
    console.log('PASS: CDP uploads and downloads, owner-only paths, no overwrite, cleanup on takeover');
  } finally {
    lines.close();
    if (gateway.exitCode === null && gateway.signalCode === null) gateway.kill('SIGTERM');
    await exited;
    await connection?.close().catch(() => {});
  }
}
