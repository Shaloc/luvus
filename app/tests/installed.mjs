// Test the installed ASAR and embedded release binary, without inheriting a
// development URL, executable override or production session.
import { useEnglishUi } from './device-language.mjs';
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { createServer } from 'node:http';
import { mkdir, mkdtemp, readFile, realpath, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { _electron } from 'playwright-core';
import { isolatedEnvironment } from '../scripts/dev.mjs';

const exec = promisify(execFile);
const repo = await realpath(resolve(dirname(fileURLToPath(import.meta.url)), '../..'));
const bundle = await realpath(process.argv.slice(2).find(x=>!x.startsWith('--')) ?? '/Applications/Luvus.app');
const exe = join(bundle, 'Contents/MacOS/Luvus'), core = join(bundle, 'Contents/Resources/luvus');
assert.equal(await realpath(exe), exe); assert.equal(await realpath(core), core);
assert.ok((await readFile(core)).equals(await readFile(join(repo, 'target/release/luvus'))), 'installed core differs from release');
assert.ok(!process.env.LUVUS_ENV, 'run outside a managed Luvus pane');
const root = await mkdtemp(join(repo, 'target/app-e2e-installed-'));
await writeFile(join(root, '.isolated-app'), root, { flag: 'wx', mode: 0o600 });
const workspace = join(root, 'work'), home = join(root, 'home'), profile = join(root, 'profile');
for (const dir of [workspace, home, profile, join(root, 'tmp')]) await mkdir(dir, { mode: 0o700 });
const session = 'installed-test';
const env = { ...isolatedEnvironment(), HOME:home, TMPDIR:join(root,'tmp'), LUVUS_HOME:home,
  XDG_DATA_HOME:join(home,'data'), XDG_CACHE_HOME:join(home,'cache'), XDG_CONFIG_HOME:join(home,'config'),
  LUVUS_APP_USER_DATA:profile, SHELL:'/bin/sh', TERM:'xterm-256color', GIT_CONFIG_NOSYSTEM:'1', GIT_CONFIG_GLOBAL:'/dev/null' };
for (const key of Object.keys(env)) if (/^(VITE_|GIT_DIR$|GIT_WORK_TREE$|GH_TOKEN$|GITHUB_TOKEN$)/.test(key) || /^(http_proxy|https_proxy|all_proxy|no_proxy)$/i.test(key)) delete env[key];
await writeFile(join(home, 'config.json'), JSON.stringify({check_updates:false, language:'en', shell:'/bin/sh'}));
const cli = async (...argv) => (await exec(core, ['--session', session, ...argv], { env, cwd:workspace, timeout:15_000, maxBuffer:4*1024*1024 })).stdout;
const acquire = async (id, ttl) => JSON.parse(await cli('browser','control','acquire','--browser',id,'--ttl',ttl)).result;
const snapshot = async () => JSON.parse(await cli('uhp', 'snapshot')).result;
const report = { bundle, core, session, root, checks:[] };
const check = name => { report.checks.push(name); console.log('PASS ' + name); };
let desktop, page;
const launch = async (offline = true) => {
  // Loopback is available for the browser fixture. Public DNS is intentionally
  // unavailable without modifying this Mac's network or firewall.
  desktop = await _electron.launch({ executablePath:exe, args:['--session',session, ...(offline ? ['--host-resolver-rules=MAP * ~NOTFOUND, EXCLUDE localhost'] : [])], cwd:workspace, env, timeout:35_000 });
  page = await desktop.firstWindow();
  await useEnglishUi(page);
  page.setDefaultTimeout(8000);
  const identity = await desktop.evaluate(({app}) => ({packaged:app.isPackaged,path:app.getAppPath(),resources:process.resourcesPath}));
  assert.ok(identity.packaged && identity.path === join(bundle,'Contents/Resources/app.asar'));
  assert.equal(identity.resources, dirname(core));
  await page.waitForFunction(() => document.querySelector('.connection-dot.online'), null, {timeout:20_000});
  assert.equal(page.url(), 'luvus-app://app/');
  assert.deepEqual(await page.evaluate(() => [typeof window.require,typeof window.process]), ['undefined','undefined']);
  report.identity = identity;
};
const origin = createServer((_req,res) => res.end('<!doctype html><title>Installed guest</title><h1>Local package</h1>'));
try {
  const iconName = (await exec('/usr/bin/plutil', ['-extract', 'CFBundleIconFile', 'raw', join(bundle, 'Contents/Info.plist')])).stdout.trim();
  assert.equal(iconName, 'icon.icns');
  const icon = await readFile(join(bundle, 'Contents/Resources', iconName));
  assert.equal(icon.subarray(0, 4).toString(), 'icns');
  assert.ok(icon.equals(await readFile(join(repo, 'app/.build/icon.icns'))));
  check('installed Finder/Dock icon matches the website-derived package icon');
  assert.match(await cli('server','status'), /not running/);
  await launch();
  await page.locator('canvas').first().waitFor();
  const first = await snapshot();
  report.generation = first.server_generation;
  check('first launch: ASAR, embedded core, sandbox and local-only resources');
  await page.locator('canvas').first().click();
  await page.keyboard.type("printf '%s%s\\n' PACKAGED_ TERMINAL");
  await page.keyboard.press('Enter');
  await page.waitForFunction(() => document.querySelector('.terminal-accessible')?.textContent.includes('PACKAGED_TERMINAL'));
  check('installed keyboard input reaches the persistent owner PTY');

  await new Promise(resolve => origin.listen(0,'127.0.0.1',resolve));
  await page.getByRole('button',{name:'New browser tab',exact:true}).click();
  await page.waitForFunction(() => document.querySelector('[aria-label="Browser address"]')?.disabled === false, null, {timeout:20_000});
  assert.equal(await page.getByLabel('Browser address').inputValue(), '');
  await page.getByLabel('Browser address').fill('http://127.0.0.1:' + origin.address().port + '/');
  await page.getByLabel('Browser address').press('Enter');
  await page.getByRole('tab',{name:'Installed guest',exact:true}).waitFor();
  const browser = JSON.parse(await cli('browser','list')).result.browsers.find(b => b.state === 'ready');
  assert.ok(browser);
  const lease = (await acquire(browser.browser_id,'30')).lease;
  const auth = ['--browser',browser.browser_id,'--lease',lease];
  await cli('browser','wait',...['--condition','{"text":"Local package"}',...auth]);
  assert.equal(JSON.parse(await cli('browser','snapshot',...auth)).result.data.title,'Installed guest');
  await cli('browser','control','release',...auth);
  check('installed embedded browser renders a loopback page and exposes the correct target');
  await page.screenshot({path:join(root,'browser.png')});
  await desktop.close(); desktop = undefined;
  assert.equal((await snapshot()).server_generation, first.server_generation);
  await launch();
  assert.equal((await snapshot()).server_generation, first.server_generation);
  assert.ok(!(JSON.parse(await cli('browser','list')).result.browsers ?? []).some(b => b.state === 'ready'));
  check('quit preserves the owner; reopen attaches to the same generation without relaunching the browser');

  for (const [width,height] of [[900,600],[1280,800],[1440,900]]) {
    await desktop.evaluate(({BrowserWindow}, [w,h]) => BrowserWindow.getAllWindows()[0].setContentSize(w,h),[width,height]);
    await page.waitForFunction(w => Math.abs(window.innerWidth-w) <= 2,width);
    const geometry = await page.evaluate(() => ({ viewport:innerWidth, content:document.documentElement.scrollWidth, dpi:devicePixelRatio }));
    assert.ok(geometry.content <= geometry.viewport + 1);
    await page.screenshot({path:join(root,width+'x'+height+'.png')});
    report.dpi = geometry.dpi;
  }
  check('installed window at 900x600, 1280x800 and 1440x900: no horizontal overflow');
  const longName='工作区-LongWorkspace'.repeat(3).slice(0,40);
  await cli('workspace','rename','0',longName);
  const label=page.locator('.workspace-main').filter({hasText:longName});await label.waitFor();
  for(const mode of ['light','dark']){
    await page.emulateMedia({colorScheme:mode});
    assert.equal(await page.evaluate(()=>getComputedStyle(document.documentElement).colorScheme),mode);
    await page.getByRole('button',{name:'Open workspace',exact:true}).first().click();
    const modal=page.getByRole('dialog');await modal.waitFor();
    const bounds=await modal.boundingBox();
    assert.ok(bounds && bounds.x>=0 && bounds.y>=0);
    await page.screenshot({path:join(root,'long-name-'+mode+'.png')});
    await modal.getByRole('button',{name:'Cancel'}).click();
    assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1));
  }
  await page.emulateMedia({colorScheme:null});
  check('light and dark native chrome, long mixed-width workspace title and modal bounds');
  if (process.argv.includes('--pressure')) {
    const metrics=async()=>{
      const rows=await desktop.evaluate(({app})=>app.getAppMetrics().map(p=>({pid:p.pid,type:p.type,processCpuPercent:p.cpu.percentCPUUsage})));
      const {stdout}=await exec('/bin/ps',['-o','pid=,rss=','-p',rows.map(r=>r.pid).join(',')],{timeout:3000});
      const rss=new Map(stdout.trim().split('\n').map(s=>s.trim().split(/\s+/).map(Number)));
      return rows.map(r=>({...r,residentKiB:rss.get(r.pid)??null}));
    };
    report.electronBefore=await metrics();
    const ready=async()=>JSON.parse(await cli('browser','list')).result.browsers.filter(b=>b.state==='ready');
    const newTab=async()=>{const before=await page.getByRole('tab').count();await page.getByRole('button',{name:'New tab',exact:true}).click();await page.waitForFunction(n=>document.querySelectorAll('[role=tab]').length===n,before+1);};
    for(let i=0;i<8;i++){
      await newTab();
      await page.getByRole('button',{name:'New browser tab',exact:true}).click();
      await page.waitForFunction(()=>document.querySelector('[aria-label="Browser address"]')?.disabled===false,null,{timeout:20_000});
      assert.equal(await page.getByLabel('Browser address').inputValue(), '');
      await page.getByLabel('Browser address').fill('http://127.0.0.1:'+origin.address().port+'/tab/'+i);
      await page.getByLabel('Browser address').press('Enter');
      assert.equal((await ready()).length,i+1);
    }
    report.electronEight=await metrics();
    await newTab();
    const targets=(await ready()).slice(0,2);
    const controls=[];
    for (const b of targets) {
      const result=await acquire(b.browser_id,'60');
      controls.push(['--browser',b.browser_id,'--lease',result.lease]);
    }
    await page.locator('canvas').first().click();
    const screen=page.locator('.terminal-accessible').first();
    report.visibleRttMs=[];
    for(let i=0;i<5;i++){
      const screenshot=Promise.all(controls.map(async auth=>{
        const response=JSON.parse(await cli('browser','screenshot',...auth));assert.ok(response.result?.data?.base64?.length>1000, JSON.stringify(response.error));
      }));
      // Observe input in a real frame while two independent screenshots are in flight.
      const marker='queued_'+i+'_echoed';
      const then=performance.now();
      await Promise.all([screenshot,(async()=>{
        await page.keyboard.type("printf '%s%s\\n' queued_"+i+" _echoed");await page.keyboard.press('Enter');
        await page.waitForFunction(s=>document.querySelector('.terminal-accessible')?.textContent.includes(s),marker);
        report.visibleRttMs.push(Math.round((performance.now()-then)*100)/100);
      })()]);
      assert.ok((await screen.textContent()).includes(marker));
    }
    for(const auth of controls)await cli('browser','control','release',...auth);
    await desktop.close();desktop=undefined;
    assert.equal((await ready()).length,0);
    assert.equal((await snapshot()).server_generation,first.server_generation);
    check('eight real guests, concurrent screenshots with visible keyboard echo, all leaves released on detach');
  }
} finally {
  try { await desktop?.close(); } finally {
    if (origin.listening) await new Promise(r => { origin.closeAllConnections(); origin.close(r); });
    assert.equal(await readFile(join(root,'.isolated-app'),'utf8'),root);
    if (!/not running/.test(await cli('server','status'))) await cli('server','stop');
    assert.match(await cli('server','status'),/not running/);
    await writeFile(join(root,'result.json'),JSON.stringify(report,null,2));
    console.log('Installed fixture: ' + root);
  }
}
