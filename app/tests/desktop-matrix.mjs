// Exercise visible controls; use the owner only to seed fixtures and
// independently verify effects. Failed cases retain their own state and screenshot.
import { useEnglishUi } from './device-language.mjs';
import assert from 'node:assert/strict';
import { execFile, spawn } from 'node:child_process';
import { once } from 'node:events';
import { chmod, mkdir, mkdtemp, readFile, realpath, rename, unlink, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { connect } from 'node:net';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { setTimeout as sleep } from 'node:timers/promises';
import { _electron } from 'playwright-core';
import { ResponseDecoder, encodeRequest } from '@luvus/app-protocol';
import { isolatedEnvironment } from '../scripts/dev.mjs';

const exec = promisify(execFile);
const repo = await realpath(resolve(dirname(fileURLToPath(import.meta.url)), '../..'));
assert.ok(!process.env.LUVUS_ENV, 'run the matrix outside a managed Luvus pane');
const bundle = process.env.LUVUS_TEST_BUNDLE;
const binary = bundle ? join(await realpath(bundle),'Contents/Resources/luvus') : join(repo, 'target/debug/luvus');
assert.equal(await realpath(binary),binary);
const root = await mkdtemp(join(repo,'target/app-e2e-matrix-'));
console.log('Desktop matrix: ' + root);
const report = { binary, root, cases:{} };
async function eventually(label, fn) {
  const end = Date.now() + 10_000;
  do { const value = await fn(); if (value) return value; await sleep(70); } while (Date.now() < end);
  throw new Error('Timeout: ' + label);
}
async function scenario(name, fn) {
  if (process.argv.length > 2 && !process.argv.slice(2).includes(name)) return;
  const home = join(root,name), workspace = join(home,'work');
  for (const path of [home,workspace,join(home,'profile'),join(home,'tmp')]) await mkdir(path, {mode:0o700});
  await writeFile(join(home,'.isolated-app'),home,{flag:'wx'});
  await writeFile(join(home,'config.json'),JSON.stringify({check_updates:false,language:'en',shell:'/bin/sh'}));
  const env = { ...isolatedEnvironment(), HOME:home, LUVUS_HOME:home, TMPDIR:join(home,'tmp'),
    SHELL:'/bin/sh', TERM:'xterm-256color', XDG_CONFIG_HOME:join(home,'config'), XDG_DATA_HOME:join(home,'data'),
    XDG_CACHE_HOME:join(home,'cache'), GIT_CONFIG_GLOBAL:'/dev/null', GIT_CONFIG_NOSYSTEM:'1', GIT_TERMINAL_PROMPT:'0',GIT_CEILING_DIRECTORIES:home };
  Object.assign(env,{CODEX_HOME:join(home,'.codex'),CLAUDE_CONFIG_DIR:join(home,'.claude'),GH_CONFIG_DIR:join(home,'.gh'),QODER_CONFIG_DIR:join(home,'.qoder')});
  for (const key of Object.keys(env)) if (/^VITE_|^GIT_(DIR|WORK_TREE|COMMON_DIR)$|^(GH|GITHUB)_TOKEN$/.test(key)) delete env[key];
  if (name==='workspace-lifecycle') { env.HOME=join(home,'user-home'); await mkdir(env.HOME); }
  if (!bundle) env.LUVUS_APP_BINARY = binary;
  const session = 'matrix';
  const cli = async (...args) => (await exec(binary,['--session',session,...args], {env,cwd:workspace,timeout:15_000,maxBuffer:4*1024*1024})).stdout;
  const api=async(method,params={})=>{
    const call=exec(binary,['--session',session,'uhp','proxy'],{env,cwd:workspace,timeout:15_000,maxBuffer:4*1024*1024});
    call.child.stdin.end(JSON.stringify({id:'matrix-proof',method,params})+'\n');
    const {stdout}=await call.catch(e=>{if(e.stdout)return {stdout:e.stdout};throw e;});
    const reply=JSON.parse(stdout);
    if(reply.error)throw Error(reply.error.message);
    return reply.result;
  };
  const snap = async () => JSON.parse(await cli('uhp','snapshot')).result;
  const clients = [];let launchCount=0;
  let page;
  const launch = async () => {
    const profile = join(home,'profile',String(launchCount++));
    await mkdir(profile);
    const native = await _electron.launch({ ...(bundle ? {executablePath:join(bundle,'Contents/MacOS/Luvus')} : {}),
      args:[...(!bundle ? [join(repo,'app/apps/desktop/dist/main.cjs')] : []),'--session',session],
      cwd:workspace,env:{...env,LUVUS_APP_USER_DATA:profile},timeout:30_000 });
    clients.push(native);
    const p = await native.firstWindow();
    await useEnglishUi(p);
    p.setDefaultTimeout(7000);
    await p.waitForFunction(() => document.querySelector('.connection-dot.online'),null,{timeout:20_000});
    return p;
  };
  const details = [];
  try {
    assert.match(await cli('server','status'),/not running/);
    if (name==='files' || name==='native-diff' || name==='github' || name==='orchestration') {
      await mkdir(join(workspace,'nested'));
      await writeFile(join(workspace,'nested','before.txt'),'fixture content');
      await writeFile(join(workspace,'notes.md'),'# Matrix document\n\nOwner-side Markdown preview.\n');
      await writeFile(join(workspace,'graph.mmd'),'flowchart TD\n  A[Start] --> B[Finish]\n');
      const git=(...args)=>exec('git',['-c','core.hooksPath=/dev/null','-c','user.name=Fixture','-c','user.email=fixture@invalid.test',...args],{cwd:workspace,env});
      await git('init','--quiet','--initial-branch=main'); await git('add','.'); await git('commit','--quiet','-m','Fixture baseline');
      await writeFile(join(workspace,'notes.md'),'# Matrix document\n\nOwner-side Markdown preview updated.\n');
      if (name==='orchestration') {
        const bin=join(home,'bin');await mkdir(bin);
        await exec('/usr/bin/cc',[join(repo,'app/tests/fixtures/composer.c'),'-o',join(bin,'codex')],{cwd:workspace,env});
        env.PATH=bin+':'+env.PATH;
      }
      if (name==='github') {
        await git('remote','add','origin','https://github.com/fixture/matrix.git');
        const bin=join(home,'bin');
        await mkdir(bin);env.PATH=bin+':/usr/bin:/bin';
        await writeFile(join(bin,'gh'),[
          '#!/bin/sh','set -eu','if [ -e "$HOME/missing" ]; then exit 127; fi',
          'case "$1 ${2:-}" in','"--version "*) echo gh-fixture;;','"auth status") exit 0;;',
          '"pr list") test ! -e "$HOME/offline" || { echo "fixture offline" >&2; exit 1; }; '+
            "echo '[{\"number\":7,\"title\":\"Fixture review\",\"author\":{\"login\":\"maintainer\"},\"state\":\"OPEN\",\"statusCheckRollup\":[{\"status\":\"COMPLETED\",\"conclusion\":\"SUCCESS\"}]}]';;",
          '"pr view") '+ "echo '{\"number\":7,\"title\":\"Fixture review\",\"body\":\"Independent detail\",\"state\":\"OPEN\",\"statusCheckRollup\":[{\"name\":\"fixture-ci\",\"status\":\"COMPLETED\",\"conclusion\":\"SUCCESS\"}]}';;",
          '*) exit 1;;','esac'
        ].join('\n'));
        await chmod(join(bin,'gh'),0o700);
      }
    }
    if(name==='agent-native'||name==='agent-recovery'||name==='desktop-ux'){
      const bin=join(home,'bin'),sessions=join(env.CODEX_HOME,'sessions','2026','11','04');
      await mkdir(bin);await mkdir(sessions,{recursive:true});env.PATH=bin+':'+env.PATH;
      env.MATRIX_AGENT_RECEIPTS=join(home,'agent-argv');
      env.MATRIX_HOLD_RESUME=join(home,'hold-resume');
      await exec('/usr/bin/cc',[join(repo,'app/tests/fixtures/composer.c'),'-o',join(bin,'codex')],{env,cwd:workspace});
      await writeFile(join(sessions,'rollout-2026-11-04T12-00-00-'+nativeId+'.jsonl'),[
        {type:'session_meta',payload:{id:nativeId,cwd:workspace}},
        {type:'turn_context',payload:{model:'fixture-model'}},
        {type:'event_msg',payload:{type:'token_count',info:{total_token_usage:{input_tokens:1600,cached_input_tokens:1200,output_tokens:180}}}}
      ].map(o=>JSON.stringify(o)).join('\n')+'\n');
    }
    page = await launch();
    const retire=async(client)=>{await client.close();const index=clients.indexOf(client);if(index>=0)clients.splice(index,1);};
    await fn({page,workspace,home,cli,api,snap,launch,retire,env,native:clients[0],ok:label=>details.push(label)});
    if(page.isClosed())page=await clients.at(-1).firstWindow();
    await page.screenshot({path:join(home,'pass.png')});
    report.cases[name] = {pass:true,checks:details};
    console.log('PASS ' + name);
  } catch (error) {
    report.cases[name] = {pass:false,checks:details,error:String(error)};
    console.error('FAIL ' + name + ': ' + String(error));
    if(page?.isClosed() && clients.at(-1)?.process().exitCode===null)page=await clients.at(-1).firstWindow().catch(()=>null);
    if (page && !page.isClosed()) {
      await page.screenshot({path:join(home,'failure.png')}).catch(()=>{});
      await writeFile(join(home,'failure.txt'),await page.locator('body').innerText().catch(()=>'closed'));
    }
  } finally {
    for (const client of clients.reverse()) await client.close();
    assert.equal(await readFile(join(home,'.isolated-app'),'utf8'),home);
    if (name==='local-sessions') {
      const other=await exec(binary,['--session','matrix-spare','server','status'],{env,cwd:workspace});
      if (!other.stdout.includes('not running')) await exec(binary,['--session','matrix-spare','server','stop'],{env,cwd:workspace});
    }
    if (!/not running/.test(await cli('server','status'))) await cli('server','stop');
    await writeFile(join(root,'result.json'),JSON.stringify(report,null,2));
  }
}
const button = (page,name) => page.getByRole('button',{name,exact:true});
const dialog = page => page.getByRole('dialog');
const submit = async (p, name) => {await button(dialog(p),name).click();await dialog(p).waitFor({state:'hidden'});};
async function choose(field, value) {
  await field.click();
  const options = field.page().getByRole('option');
  await (typeof value === 'object' ? options.nth(value.index) : field.page().locator('[role="option"][data-value=' + JSON.stringify(value) + ']')).click();
}
const active = snap => snap.workspaces.find(w=>w.active);
const tab = snap => active(snap).tabs.find(t=>t.active);
const ids = snap => tab(snap).panes.map(p=>p.pane_id);
const nativeId='00000000-0000-4000-8000-000000000117';
async function palette(p,label) {
  await button(p,'Find a command… ⌘ K').click();
  await p.getByRole('combobox',{name:'Find command or workspace'}).fill(label);
  await p.getByRole('option').filter({hasText:label}).first().click();
}
async function openWorkspace(p,path) {
  await button(p,'Open workspace').first().click();
  await dialog(p).getByRole('textbox').fill(path);
  await submit(p,'Open workspace');
}

await scenario('layout-history',async ({page:p,snap,ok})=>{
  await p.locator('canvas').first().waitFor();
  const a = ids(await snap())[0];
  await button(p,'Split active pane').click();
  assert.equal(await dialog(p).count(),0);
  await eventually('two real panes',async()=>ids(await snap()).length === 2);
  const b = ids(await snap()).find(id=>id!==a);
  const pane = () => p.locator('.pane-focused');
  const menu = async () => { await pane().getByLabel('Pane actions').click(); };
  await pane().getByRole('button',{name:'Zoom pane',exact:true}).click();
  await eventually('single zoomed canvas',async()=>await p.locator('canvas').count()===1);
  await button(pane(),'Unzoom pane').click();
  await eventually('two canvases',async()=>await p.locator('canvas').count()===2);
  ok('zoom and unzoom preserve both terminal IDs');
  await menu(); await button(pane(),'Swap with pane…').click();
  await choose(dialog(p).getByRole('combobox'),a);
  await submit(p,'Apply');
  await eventually('swapped order',async()=>ids(await snap())[0]===b);
  if (!await button(pane(),'Transpose split').isVisible()) await menu();
  await button(pane(),'Transpose split').click();
  await p.getByRole('separator',{name:'Resize split'}).filter({visible:true}).waitFor();
  await eventually('transposed axis',async()=>await p.getByRole('separator',{name:'Resize split'}).getAttribute('aria-orientation')==='horizontal');
  ok('swap and transpose mutate the owner layout');
  if (!await button(pane(),'Move to tab…').isVisible()) await menu();
  await button(pane(),'Move to tab…').click(); await submit(p,'Apply');
  await eventually('moved pane',async()=>active(await snap()).tabs.length===2);
  const moved = await snap();
  assert.ok(active(moved).tabs.some(t=>t.panes.length===1 && t.panes[0].pane_id===b));
  ok('pane moves to a new tab without changing identity');
  await p.getByRole('tab').first().click();
  await eventually('first tab selected',async()=>ids(await snap())[0]===a && await p.getByRole('tab').first().getAttribute('aria-selected')==='true');
  await p.getByRole('tab').nth(1).click();
  await eventually('second tab selected',async()=>ids(await snap())[0]===b && await p.getByRole('tab').nth(1).getAttribute('aria-selected')==='true');
  await palette(p,'Focus back');
  await eventually('focus history back',async()=>ids(await snap())[0]===a);
  await palette(p,'Focus forward');
  await eventually('focus history forward',async()=>ids(await snap())[0]===b);
  ok('back and forward select the intended owner pane across tabs');
});

await scenario('workspace-lifecycle',async ({page:p,home,workspace,snap,cli,ok})=>{
  const other=join(home,'other'), original=active(await snap()).id;
  await mkdir(other); await openWorkspace(p,other);
  await eventually('other focused',async()=>active(await snap()).cwd===other);
  const otherId=active(await snap()).id;
  const first=p.locator('.workspace-row').filter({has:p.locator('button[title="'+workspace+'"]')});
  await first.locator('summary').click();
  await button(first,'Rename…').click();
  await dialog(p).getByRole('textbox').fill('original-only'); await submit(p,'Apply');
  assert.equal((await snap()).workspaces.find(w=>w.id===original).name,'original-only');
  assert.equal(active(await snap()).id,otherId);
  ok('background workspace rename does not mutate or focus the foreground workspace');
  if (!await button(first,'Close workspace…').isVisible()) await first.locator('summary').click();
  await button(first,'Close workspace…').click(); await button(dialog(p),'Cancel').click();
  assert.equal((await snap()).workspaces.length,2);
  await button(first,'Close workspace…').click(); await submit(p,'Close workspace');
  await eventually('no original workspace',async()=>(await snap()).workspaces.length===1);
  // The core intentionally creates a replacement shell in HOME when it
  // exists. Exercise the legitimate zero-workspace state with no fallback.
  await rename(join(home,'user-home'), join(home,'unavailable-home'));
  const last = p.locator('.workspace-row').first();
  await last.locator('summary').click(); await button(last,'Close workspace…').click(); await submit(p,'Close workspace');
  await p.getByText('Your workspace starts here',{exact:true}).waitFor();
  assert.equal((await snap()).workspaces.length,0);
  ok('cancel, explicit close, and a live server with zero workspaces');
  await openWorkspace(p,workspace);
  await eventually('reopen',async()=>active(await snap())?.cwd===workspace);
  assert.match(await cli('server','status'),/running/);
  ok('open from empty state creates a live terminal');
});

await scenario('settings-modules',async ({page:p,home,ok})=>{
  await p.locator('.sidebar-bottom').getByRole('button',{name:'Settings'}).click();
  await p.getByLabel('Find setting').fill('persist_pane_screen');
  const toggle=p.getByRole('switch',{name:'session.persist_pane_screen'});
  await toggle.click();
  await eventually('persist option stored',async()=>JSON.parse(await readFile(join(home,'config.json'),'utf8')).session?.persist_pane_screen===false);
  await button(p,'Keybindings').click();
  await p.locator('.details dt').filter({hasText:/^prefix$/i}).first().waitFor();
  await button(p,'Appearance').click();
  await p.getByRole('combobox',{name:'Terminal theme',exact:true}).waitFor();
  ok('saving a setting changes the owner; effective keys and installed themes render');
  await button(p,'Back to terminals').click();
  await button(p,'Modules').click();
  await button(p,'Link module…').click();
  await dialog(p).getByRole('textbox').fill('/path-that-does-not-exist/luvus-module-test');
  await button(dialog(p),'Link module').click(); await dialog(p).getByRole('alert').waitFor();
  ok('invalid module link shows an error without closing the terminal');
  await button(dialog(p),'Cancel').click();
});

await scenario('desktop-ux',async ({page:p,native,home,api,snap,ok})=>{
  await native.evaluate(({BrowserWindow})=>BrowserWindow.getAllWindows()[0].setContentSize(1280,820));
  await p.getByLabel('Terminal input').fill('codex');
  await p.getByLabel('Terminal input').press('Enter');
  await p.locator('.agent-row').first().waitFor();
  const checkDot=async()=>{
    const rect=await p.locator('.agent-main .status-dot').first().boundingBox();
    assert.equal(rect.width,6);assert.equal(rect.height,6);
  };
  await checkDot();
  const footer = p.locator('.sidebar-bottom'), settings = button(footer,'Settings');
  const actions = [settings,button(footer,'Mission Control'),button(footer,'Orchestration')];
  const background = locator => locator.evaluate(e=>getComputedStyle(e).backgroundColor);
  await p.emulateMedia({colorScheme:'dark'});
  await p.mouse.move(500,42);
  const idle = await Promise.all(actions.map(background));
  for (const [i, action] of actions.entries()) {
    await eventually('only the hovered footer action highlights: '+i,async()=>{
      await action.hover();
      return await background(action)!==idle[i];
    });
    const colors = await Promise.all(actions.map(background));
    assert.deepEqual(colors.filter((_,j)=>j!==i),idle.filter((_,j)=>j!==i));
  }
  await p.screenshot({path:join(home,'footer-hover.png'),clip:await footer.boundingBox()});
  await p.emulateMedia({colorScheme:null});
  assert.equal(await p.locator('.workspace-section').count(),1);
  assert.equal(await p.locator('.agent-section').count(),1);
  await p.getByRole('combobox',{name:'Filter agents by state'}).click();
  await p.getByRole('option',{name:'Working',exact:true}).waitFor();
  await p.screenshot({path:join(home,'state-menu.png')});
  await p.keyboard.press('Escape');
  await p.getByRole('button',{name:'Switch session',exact:true}).click();
  await p.getByLabel('Find session').waitFor();
  await p.locator('.session-option').filter({hasText:'matrix'}).waitFor();
  assert.equal(await p.locator('.full-tool').count(),0);
  assert.ok(await p.locator('.terminal-surface canvas').isVisible());
  await p.screenshot({path:join(home,'session-picker.png')});
  await p.keyboard.press('Escape');
  const count=active(await snap()).tabs.length;
  await button(p,'New tab').click();
  await eventually('new tab without naming prompt',async()=>active(await snap()).tabs.length===count+1);
  assert.equal(await p.locator('dialog[open]').count(),0);
  await p.getByLabel('Terminal input').click();
  await p.keyboard.press('Meta+t');
  await eventually('terminal shortcut opens tab',async()=>active(await snap()).tabs.length===count+2);
  assert.equal(await p.locator('dialog[open]').count(),0);
  await button(p,'Split active pane').click();
  await eventually('automatic split without a dialog',async()=>ids(await snap()).length===2 && await p.locator('.pane').count()===2);
  assert.equal(await dialog(p).count(),0);
  await p.screenshot({path:join(home,'automatic-split.png')});
  await native.evaluate(({Menu})=>{
    const popup=Menu.prototype.popup;
    Menu.prototype.popup=function(options){
      globalThis.menuLabels=this.items.map(i=>i.label);
      globalThis.dismissMenu=()=>this.closePopup(options.window);
      Menu.prototype.popup=popup;
      return popup.call(this,options);
    };
  });
  const surface=p.locator('.pane-focused canvas').first();
  await surface.click({button:'right'});
  await eventually('native terminal context menu',async()=>JSON.stringify(await native.evaluate(()=>globalThis.menuLabels))==='["Copy","Paste"]');
  await native.evaluate(()=>globalThis.dismissMenu());
  await surface.click();
  const input=p.locator('.pane-focused').getByLabel('Terminal input');
  await input.fill("printf '\\033[48;2;109;83;61m%36s\\033[0m\\n' ''");
  await input.press('Enter');
  await p.waitForFunction(()=> {
    const canvas=document.querySelector('.pane-focused canvas');
    const {width:w,height:h}=canvas, ctx=canvas.getContext('2d');
    const data=ctx.getImageData(0,0,w,h).data;
    const minimum=36*ctx.measureText('M').width*devicePixelRatio-3;
    for(let y=0;y<h;y+=2){
      let streak=0;
      for(let x=0;x<w;x++){const i=4*(w*y+x);streak=data[i]===109 && data[i+1]===83 && data[i+2]===61?streak+1:0;if(streak>=minimum)return true;}
    }
    return false;
  },null,{timeout:7000});
  ok('native right-click menu, continuous true-color terminal background at physical-pixel resolution');
  await p.locator('.pane-focused').getByRole('button',{name:'Copy on selection',exact:true}).click();
  await eventually('copy toggle owner snapshot',async()=>!(await p.locator('.pane-focused').getByRole('button',{name:'Copy on selection'}).getAttribute('aria-pressed')==='true'));
  await button(p.locator('.pane-focused'),'Right-click passthrough').click();
  await eventually('mouse toggle owner snapshot',async()=>await p.locator('.pane-focused').getByRole('button',{name:'Right-click passthrough'}).getAttribute('aria-pressed')==='true');
  const activeTab=active(await snap()).tabs.find(t=>t.active);
  await api('tab.rename',{tab_id:activeTab.id,name:'agent-rename'});
  await p.getByRole('tab',{name:'agent-rename',exact:true}).waitFor();
  await p.locator('.pane-focused').getByLabel('Pane actions').click();
  await button(p.locator('.pane-focused'),'Close pane').click();
  await eventually('one-click close',async()=>await p.locator('.pane').count()===1);
  assert.equal(await dialog(p).count(),0);
  ok('split sidebar, fixed status dots, styled filters, compact session picker, immediate tabs and automatic splits, core mouse toggles');
  await p.locator('.sidebar-bottom').getByRole('button',{name:'Settings',exact:true}).click();
  await choose(p.getByRole('combobox',{name:'Interface language',exact:true}),'zh');
  await p.getByRole('heading',{name:'通用',exact:true}).waitFor();
  await p.getByRole('button',{name:'添加主机'}).waitFor();
  await p.screenshot({path:join(home,'settings-zh.png')});
  await choose(p.getByRole('combobox',{name:'界面语言',exact:true}),'en');
  await p.getByRole('combobox',{name:'Core language',exact:true}).waitFor();
  await choose(p.getByRole('combobox',{name:'notifications.display',exact:true}),'inbox');
  await eventually('notification preference persisted',async()=> JSON.parse(await readFile(join(home,'config.json'),'utf8')).notifications?.display==='inbox');
  await button(p,'Appearance').click();
  await button(p,'Dark').click();
  assert.equal(await p.evaluate(()=>document.documentElement.dataset.mode),'dark');
  await p.getByLabel('Interface font size').fill('14');
  await p.getByLabel('Background image',{exact:true}).setInputFiles(join(repo,'website/public/brand/luvus-logo.png'));
  await p.getByAltText('Selected background').waitFor();
  await p.getByLabel('Background visibility').fill('27');
  await p.screenshot({path:join(home,'appearance-dark.png')});
  await button(p,'Light').click();
  await p.screenshot({path:join(home,'appearance-light.png')});
  await button(p,'Terminal').click();
  await p.getByLabel('Terminal font size',{exact:true}).fill('17');
  await button(p,'Back to terminals').click();
  assert.equal(await p.getByLabel('Terminal input').first().evaluate(e=>getComputedStyle(e).fontSize),'17px');
  await p.reload();
  await p.waitForFunction(()=>document.querySelector('.connection-dot.online'));
  assert.equal(await p.evaluate(()=>document.documentElement.dataset.mode),'light');
  await p.waitForFunction(()=>document.documentElement.dataset.wallpaper==='true');
  assert.equal(await p.getByLabel('Terminal input').first().evaluate(e=>getComputedStyle(e).fontSize),'17px');
  await checkDot();
  await native.evaluate(({BrowserWindow})=>BrowserWindow.getAllWindows()[0].setContentSize(900,650));
  await checkDot();
  await p.screenshot({path:join(home,'sidebar-compact.png')});
  ok('categorized settings, visible language, live fonts, light/dark mode, local wallpaper survives reload, compact layout');
});

await scenario('settings-inventory',async ({page:p,native,home,api,env,ok})=>{
  await mkdir(env.CODEX_HOME,{recursive:true});
  const hooks=join(env.CODEX_HOME,'hooks.json');
  await writeFile(hooks,JSON.stringify({hooks:{},fixture_unrelated:'keep'}),{flag:'wx'});
  await p.locator('.sidebar-bottom').getByRole('button',{name:'Settings'}).click();
  const config=await p.evaluate(async()=>(await window.luvus.bootstrap()).find(m=>m.type==='snapshot').snapshot.config);
  const paths=(o,prefix='')=>Object.entries(o).flatMap(([k,v])=>{const name=prefix?prefix+'.'+k:k;return v && typeof v==='object' && !Array.isArray(v) && Object.keys(v).length?paths(v,name):[name];});
  const rendered=new Set();
  for (const key of Object.keys(config)) {
    await p.getByLabel('Find setting').fill(key);
    for (const path of await p.locator('.settings-content [aria-label]').evaluateAll(nodes=>nodes.map(n=>n.getAttribute('aria-label')))) rendered.add(path);
  }
  assert.ok(rendered.delete('Interface language')); assert.ok(rendered.delete('Core language')); rendered.add('language');
  assert.deepEqual([...rendered].sort(),paths(config).sort());
  await button(p,'General').click();
  for(const lang of ['zh','ja','ko','de','fr','pt','es','id','en']){
    await choose(p.getByRole('combobox',{name:'Core language',exact:true}),lang);
    await eventually('owner language '+lang,async()=>JSON.parse(await readFile(join(home,'config.json'),'utf8')).language===lang);
  }
  ok('every projected core preference has a control; all nine registered language settings persist');
  await button(p,'Integrations').click();
  const codex=p.locator('[data-agent=codex]');await codex.waitFor();
  const publicRows=(await api('integration.status')).integrations;
  assert.equal(await p.locator('[data-agent]').count(),publicRows.length);
  await native.evaluate(({dialog})=>{
    dialog.showMessageBox=async(_win,opts)=>{
      if(opts.cancelId!==0 || opts.defaultId!==0)throw Error('unsafe add-on confirmation');
      const response=globalThis.fixtureAllowAddon===true?1:0;globalThis.fixtureAllowAddon=false;
      return {response,checkboxChecked:false};
    };
  });
  await button(codex,'Install…').click();assert.ok(!(await api('integration.status')).integrations.find(row=>row.agent==='codex').installed);
  const allow=()=>native.evaluate(()=>{globalThis.fixtureAllowAddon=true;});
  await allow();await button(codex,'Install…').click();await button(codex,'Uninstall…').waitFor();
  assert.ok((await api('integration.status')).integrations.find(row=>row.agent==='codex').installed);
  await allow();await button(codex,'Uninstall…').click();await button(codex,'Install…').waitFor();
  assert.equal(JSON.parse(await readFile(hooks,'utf8')).fixture_unrelated,'keep');
  const skill=await api('skill.status');
  assert.ok(skill.destinations.length && skill.destinations.every(row=>row.target.startsWith(home+'/')),'never touch nonfixture agent directories');
  const shared=p.locator('.setting-row').filter({has:p.getByText('shared',{exact:true})});
  await allow();await button(p,'Enable skill…').click();
  await shared.getByText('current',{exact:true}).waitFor();assert.ok((await api('skill.status')).destinations.some(row=>row.state==='current'));
  await allow();await button(p,'Disable skill…').click();
  await shared.getByText('not-installed',{exact:true}).waitFor();assert.ok((await api('skill.status')).destinations.every(row=>row.state!=='current'));
  ok('local integration inventory, cancel/install/uninstall, unrelated Codex hooks survive; bundled skills enabled and disabled only in marked agent homes');
});

await scenario('commander-private',async ({page:p,launch,ok})=>{
  const q=await launch();
  await button(p,'Toggle Commander').click(); await button(q,'Toggle Commander').click();
  const draft=p.getByRole('textbox',{name:'Commander prompt'});
  await draft.fill('draft for display one');
  assert.equal(await q.getByRole('textbox',{name:'Commander prompt'}).inputValue(),'');
  await button(p,'Close Commander').click(); await button(p,'Toggle Commander').click();
  assert.equal(await draft.inputValue(),'draft for display one');
  assert.ok(await button(p,'Review & send').isDisabled());
  ok('two attached desktop clients retain separate drafts; empty target sets cannot send');
});

await scenario('terminal-ime',async ({page:p,workspace,native,cli,snap,ok})=>{
  const pane=ids(await snap())[0];
  await writeFile(join(workspace,'ime.py'),[
    'import sys','print("MATRIX_IME_ARMED",flush=True)',
    'line=input()','print("ORD="+",".join(str(ord(c)) for c in line),flush=True)',
    'print("\\x1b]8;;https://example.invalid/matrix\\aMATRIX_LINK\\x1b]8;;\\a",flush=True)',
    'print("\\x1b]52;c;bWF0cml4LWNsaXBib2FyZA==\\a",flush=True)',
  ].join('\n'));
  // Capture the native write boundary in this fixture only. Do not read,
  // clear or replace the user's system pasteboard to perform an acceptance test.
  await native.evaluate(({clipboard})=>{clipboard.writeText=s=>{globalThis.matrixClipboard=s;};});
  await p.locator('canvas').first().click(); await p.keyboard.type('python3 ime.py'); await p.keyboard.press('Enter');
  const read=async()=>JSON.parse(await cli('pane','read',pane)).result.text;
  await eventually('input program listening',async()=>(await read()).includes('MATRIX_IME_ARMED'));
  const devtools=await p.context().newCDPSession(p);
  await devtools.send('Input.imeSetComposition',{text:'拼',selectionStart:1,selectionEnd:1});
  assert.ok(!(await read()).includes('拼'),'preedit leaked into PTY');
  await devtools.send('Input.insertText',{text:'中e\u0301界🙂'});
  await p.keyboard.press('Enter');
  await eventually('exact Unicode commit',async()=>(await read()).includes('ORD=20013,101,769,30028,128578'));
  await devtools.detach();
  await eventually('OSC8 link target',()=>p.evaluate(async()=>(await window.luvus.bootstrap()).some(m=>m.type==='terminal' && m.frame.cells.some(c=>c.link==='https://example.invalid/matrix'))));
  await eventually('OSC52 native receipt',()=>native.evaluate(()=>globalThis.matrixClipboard==='matrix-clipboard'));
  ok('Chromium IME preedit stays local, exact wide/combining/emoji commit reaches PTY once; OSC 8 and OSC 52 retain their target');
});

await scenario('terminal-selection',async ({page:p,workspace,native,ok})=>{
  await writeFile(join(workspace,'wrapped.py'),[
    'import os,sys','c=os.get_terminal_size().columns',
    'sys.stdout.write("\\x1b[2J\\x1b[H  HEAD"+"x"*(c-6)+"界tail\\n  END");sys.stdout.flush();input()'
  ].join('\n'));
  await native.evaluate(({clipboard})=>{clipboard.writeText=s=>{globalThis.copyReceipt=s};});
  const canvas=p.locator('canvas').first();
  await canvas.click();await p.keyboard.type('python3 wrapped.py');await p.keyboard.press('Enter');
  const frame=()=>p.evaluate(async()=>(await window.luvus.bootstrap()).find(m=>m.type==='terminal')?.frame);
  const f=await eventually('owner wrap flag',async()=>{const f=await frame();return f?.wraps[0] && f.cells.some(c=>c.row===2 && c.text==='E') && f;});
  const rect=await canvas.boundingBox(), cw=Math.floor(rect.width/f.cols), ch=Math.floor(rect.height/f.rows);
  await p.mouse.move(rect.x+cw/2,rect.y+ch/2);await p.mouse.down();
  await p.mouse.move(rect.x+4.5*cw,rect.y+2.5*ch,{steps:8});await p.mouse.up();await p.keyboard.press('Meta+c');
  const expected='  HEAD'+'x'.repeat(f.cols-6)+'界tail\n  END';
  await eventually('soft wrap joins; hard line and indent survive copy',async()=>await native.evaluate(()=>globalThis.copyReceipt)===expected);
  await native.evaluate(({BrowserWindow})=>BrowserWindow.getAllWindows()[0].setContentSize(1000,700));
  const next=await eventually('new geometry',async()=>{const n=await frame();return n?.geometry!==f.geometry && n;});
  for(const input of [{kind:'selection',mode:'cell',geometry:f.geometry,revision:f.revision,start:[0,0],end:[0,1]},
    {kind:'mouse',geometry:f.geometry,action:'press',button:0,row:0,col:1}]){
    await assert.rejects(p.evaluate(({target,input})=>window.luvus.request({type:'input',id:crypto.randomUUID(),target,input}),{target:next.target,input}),/geometry|stale/i);
  }
  assert.equal(await native.evaluate(()=>globalThis.copyReceipt),expected);
  ok('physical drag and Cmd+C preserve wide cells, indentation and soft wraps; stale geometry rejects both pointer and selection');
});

await scenario('terminal-graphics',async ({page:p,workspace,ok})=>{
  await writeFile(join(workspace,'pixels.py'),[
    'import sys,base64','write=sys.stdout.write',
    'pixels=base64.b64encode(bytes([17,231,83])*256).decode()',
    'write("\\x1b[?1049h\\x1b[3;4H\\x1b_Ga=T,f=24,s=16,v=16,i=37,c=4,r=2,C=1;"+pixels+"\\x1b\\\\");sys.stdout.flush()',
    'input()', 'write("\\x1b_Ga=d,d=a\\x1b\\\\\\x1b[?1049l");sys.stdout.flush()',
  ].join('\n'));
  await p.locator('canvas').first().click();await p.keyboard.type('python3 pixels.py');await p.keyboard.press('Enter');
  const count=()=>p.locator('canvas').first().evaluate(canvas=>{
    const ctx=canvas.getContext('2d'),data=ctx.getImageData(0,0,canvas.width,canvas.height).data;
    let n=0;for(let i=0;i<data.length;i+=16)if(data[i]===17 && data[i+1]===231 && data[i+2]===83)n++;
    return n;
  });
  await eventually('Kitty image pixels',async()=>await count()>100);
  await p.screenshot({path:join(workspace,'kitty.png')});
  await p.keyboard.press('Enter');
  await eventually('deleted image retires pixels',async()=>await count()===0);
  ok('raw Kitty pixels reach the visible canvas; alternate-screen exit and image deletion retire the resource');
});

await scenario('files',async ({page:p,workspace,ok})=>{
  await button(p,'Files').click();
  const files=p.locator('.files-panel'),filter=p.getByLabel('Filter loaded files');
  await button(files,'nested').click();
  const row=name=>files.locator('.file-tree-row').filter({has:files.page().getByTitle(join(workspace,'nested',name),{exact:true})});
  const before=row('before.txt');
  await before.waitFor();
  await filter.fill('before');
  assert.equal(await files.locator('.file-tree-row').count(),1);
  await filter.fill('');
  await before.locator('summary').click(); await button(before,'Rename…').click();
  await dialog(p).getByRole('textbox').fill('after.txt'); await submit(p,'Rename');
  assert.equal(await readFile(join(workspace,'nested/after.txt'),'utf8'),'fixture content');
  ok('folder expansion, filtering, confirmed rename and preserved bytes');
  const after=row('after.txt'); await after.waitFor();
  await after.locator('summary').click(); await button(after,'Delete…').click();
  await button(dialog(p),'Cancel').click();
  assert.equal(await readFile(join(workspace,'nested/after.txt'),'utf8'),'fixture content');
  await button(after,'Delete…').click();
  await dialog(p).waitFor();
  await rename(join(workspace,'nested/after.txt'),join(workspace,'nested/original.txt'));
  await writeFile(join(workspace,'nested/after.txt'),'replacement');
  await button(dialog(p),'Delete entry').click();
  await dialog(p).getByRole('alert').filter({hasText:'changed'}).waitFor();
  assert.equal(await readFile(join(workspace,'nested/after.txt'),'utf8'),'replacement');
  await button(dialog(p),'Cancel').click();
  await button(after,'Delete…').click(); await submit(p,'Delete entry');
  await assert.rejects(readFile(join(workspace,'nested/after.txt')),{code:'ENOENT'});
  assert.equal(await readFile(join(workspace,'nested/original.txt'),'utf8'),'fixture content');
  ok('cancel preserves the file, stale confirmation refuses replacement, fresh confirmation deletes only the named entry');
  await button(files,'New entry…').click();
  await choose(dialog(p).getByRole('combobox'),'create_directory');
  await dialog(p).getByRole('textbox').fill('created');await submit(p,'Create');
  const folder=files.locator('.file-tree-row').filter({has:p.getByTitle(join(workspace,'created'),{exact:true})});
  await folder.locator('summary').click();await button(folder,'New entry here…').click();
  await dialog(p).getByRole('textbox').fill('lookup-target.txt');await submit(p,'Create');
  assert.equal(await readFile(join(workspace,'created/lookup-target.txt'),'utf8'),'');
  ok('create folder and new file within the chosen directory');
  await p.keyboard.press('Meta+p');
  await choose(p.getByRole('combobox',{name:'Search scope'}),'files');
  await p.getByRole('textbox',{name:'Search query'}).fill('lookup-target');
  const result=p.locator('.search-result').filter({hasText:'lookup-target'});
  await eventually('finder indexes new entry',async()=>{
    if (await result.count()) return true;
    if (await button(p,'Search').isEnabled()) await button(p,'Search').click();
    return false;
  });
  await result.first().click();
  await eventually('finder activated in same workspace',async()=>(await p.locator('.pane-header').allTextContents()).some(s=>s.includes('lookup-target')));
  ok('finder activates the indexed path in the selected workspace');
});

await scenario('native-diff',async ({page:p,workspace,cli,ok})=>{
  await button(p,'Files').click();
  for (const [name,selector] of [['notes.md','.document-view [role=heading]'],['graph.mmd','.flowchart']]) {
    const row=p.locator('.file-tree-row').filter({has:p.getByTitle(join(workspace,name),{exact:true})});
    await row.locator('summary').click(); await button(row,'Open document preview').click();
    await p.locator(selector).first().waitFor();
  }
  assert.match(await p.locator('.flowchart').textContent(),/Start.*Finish/);
  ok('Markdown and Mermaid files render native core headings and flow nodes');
  await button(p,'Source control').click();
  await p.getByRole('tab',{name:'History',exact:true}).click();
  await p.locator('.commit-row').filter({hasText:'Fixture baseline'}).click();
  await dialog(p).getByText('Fixture baseline').waitFor(); await submit(p,'Close');
  ok('real local commit detail in an isolated repository');
  await button(p,'Diff review').click();
  await p.locator('.change-list .change-row').filter({hasText:'notes.md'}).click();
  await button(p,'Add note…').click();
  await dialog(p).getByRole('spinbutton').fill('3'); await dialog(p).getByRole('textbox').fill('matrix review');
  await submit(p,'Save note');
  const note=p.locator('.review-notes .task-card').filter({hasText:'matrix review'});
  await note.waitFor();
  await button(note,'Resolve…').click(); await submit(p,'Confirm');
  await button(note,'Reopen…').waitFor();
  ok('diff entry, positioned review note, saved body and state transition');
  await button(note,'Edit…').click();await dialog(p).getByRole('textbox').fill('stale draft');
  const current=JSON.parse(await cli('diff','note','list','--file','notes.md')).result.notes[0];
  await cli('diff','note','edit',current.id,'--body','second reviewer saved','--if-note-revision',String(current.revision));
  await button(dialog(p),'Apply').click();
  await dialog(p).getByText(/note changed/i).waitFor();
  assert.equal(await dialog(p).getByRole('textbox').inputValue(),'stale draft');
  assert.equal(JSON.parse(await cli('diff','note','list')).result.notes[0].body,'second reviewer saved');
  await button(dialog(p),'Cancel').click();
  await button(p,'Open split diff').click();
  await p.locator('.split-diff-row [data-side="old"]').filter({hasText:'Owner-side Markdown preview.'}).first().waitFor();
  await p.locator('.split-diff-row [data-side="new"]').filter({hasText:'Owner-side Markdown preview updated.'}).first().waitFor();
  ok('stale editor keeps its draft without overwriting a newer revision; native side-by-side cells retain the exact old/new lines');
});

await scenario('github',async ({page:p,home,ok})=>{
  await button(p,'Source control').click(); await p.getByRole('tab',{name:'Pull requests'}).click();
  await p.locator('.commit-row').filter({hasText:'Fixture review'}).click();
  await p.getByText('Independent detail',{exact:true}).waitFor();await p.getByText('fixture-ci',{exact:true}).waitFor();
  await p.getByText('passing',{exact:true}).waitFor();
  ok('core gh parser projects PR detail and passing check from a deterministic provider');
  await writeFile(join(home,'offline'),'');await button(p,'Back to pull requests').click();
  await p.getByRole('alert').filter({hasText:'fixture offline'}).waitFor();
  await writeFile(join(home,'missing'),'');await button(p,'Refresh Git').click();
  await p.getByRole('alert').filter({hasText:'install GitHub CLI'}).waitFor();
  ok('offline and unavailable gh remain explicit; no credentials or live repository used');
});

await scenario('orchestration',async ({page:p,workspace,home,cli,snap,ok})=>{
  const parent=active(await snap()).id;
  await button(p,'Orchestration').click(); await button(p,'New task').click();
  await dialog(p).getByRole('textbox',{name:'Task title'}).fill('Fixture worker');
  await dialog(p).getByRole('textbox',{name:'Briefing'}).fill('Run offline fixture');
  await dialog(p).getByRole('textbox',{name:'Quality gate command'}).fill('test -f approved-gate || { echo gate-unapproved; exit 13; }');
  await submit(p,'Create task');
  const task=p.locator('.task-card').filter({hasText:'Fixture worker'});
  await task.waitFor(); await button(task,'Start').click();
  await dialog(p).getByRole('textbox',{name:'Agent kind'}).fill('codex');
  await choose(dialog(p).getByRole('combobox',{name:'Execution mode'}),'worktree');
  await submit(p,'Start worker');
  const tasks=async()=>JSON.parse(await cli('task','list')).result.tasks;
  const live=await eventually('worktree worker',async()=>{
    const t=(await tasks()).find(t=>t.title==='Fixture worker');
    return t?.worktree && t;
  });
  assert.ok(JSON.stringify(live).includes(parent));
  const linked=(await snap()).workspaces.find(w=>w.cwd!==workspace && w.cwd.startsWith(home+'/'));
  assert.ok(linked,'worker must stay in the isolated fixture');
  ok('task worker starts in an isolated worktree owned by the explicit parent workspace');
  await button(task,'Done').click();await submit(p,'Confirm');
  await eventually('failed gate holds task in review',async()=>(await tasks()).some(t=>t.id===live.id && t.status==='review' && JSON.stringify(t.outputs).includes('gate-unapproved')));
  await writeFile(join(linked.cwd,'approved-gate'),'pass',{flag:'wx'});
  await button(p,'Refresh orchestration').click();await button(task,'Done').click();await submit(p,'Confirm');
  await eventually('task completion',async()=>(await tasks()).some(t=>t.title==='Fixture worker' && t.status==='done'));
  await p.getByRole('tab',{name:'leases',exact:true}).click();
  await p.getByRole('tab',{name:'worktrees',exact:true}).click();
  await eventually('worktree display',async()=>(await p.locator('.full-tool').innerText()).includes(linked.cwd));
  ok('owner task completion and worktree list are reflected by the desktop');
});

await scenario('agent-native',async ({page:p,home,api,ok})=>{
  const receipt=async()=>readFile(join(home,'agent-argv'),'utf8').catch(()=>'');
  await button(p,'Agent details').click();await p.getByRole('tab',{name:'history',exact:true}).click();
  const row=p.locator('.list-card').filter({hasText:nativeId});
  await eventually('history from marked agent home',async()=>{if(await row.isVisible())return true;await button(p,'Refresh agent data').click();return false;});
  await button(row,'Resume').click();await submit(p,'Resume');
  await eventually('native resume invocation',async()=>(await receipt()).includes('resume '+nativeId));
  const agent=p.locator('.agent-row').filter({hasText:/codex/i}).first();
  await agent.locator('button').last().click();
  await button(p,'Fork…').click();await submit(p,'Fork session');
  await eventually('fork of original session into another PTY',async()=>(await receipt()).includes('fork '+nativeId));
  ok('isolated native store discovered by core; GUI resume and fork launch the reviewed adapter argv on separate real PTYs');
  await button(p,'Mission Control').click();await button(p,'Refresh usage').click();
  await eventually('persisted native counts in fleet',async()=>{
    if((await p.locator('.fleet-row').allTextContents()).some(row=>row.includes('580')))return true;
    await button(p,'Refresh usage').click();return false;
  });
  const mission=await api('mission.snapshot');
  assert.ok(mission.rows.some(r=>r.usage?.total_tokens===580));
  ok('Mission Control and owner both report 580 non-cache tokens: 1,600 input minus 1,200 cached, plus 180 output');
});

await scenario('agent-recovery',async ({page:first,native,home,workspace,snap,api,cli,launch,retire,env,ok})=>{
  let p=first;
  const history=async sid=>{
    await button(p,'Agent details').click();await p.getByRole('tab',{name:'history',exact:true}).click();
    const row=p.locator('.list-card').filter({hasText:sid});
    await eventually('find resume row',async()=>{if(await row.isVisible())return true;await button(p,'Refresh agent data').click();return false;});
    await button(row,'Resume').click();await submit(p,'Resume');
  };
  await history(nativeId);
  await p.locator('.agent-row').filter({hasText:/codex/i}).first().waitFor();
  await button(p,'Orchestration').click();await p.getByRole('tab',{name:'automations'}).click();await button(p,'Schedule agent prompt…').click();
  const d=dialog(p);await d.getByLabel('Name',{exact:false}).fill('Durable check');await d.getByLabel('Prompt').fill('recovered-receipt');
  await choose(d.getByLabel('Agent terminal'),{index:1});await submit(p,'Schedule');
  const definition=async()=>(await api('automation.list')).automations.find(a=>a.name==='Durable check');
  const before=await definition();
  assert.equal(before.target.binding,'durable');assert.ok(!JSON.stringify(before).includes(nativeId));
  const generation=(await snap()).server_generation,old=before.target.terminal_id;
  await writeFile(env.MATRIX_HOLD_RESUME,'hold fresh readiness',{flag:'wx'});
  await retire(native);await cli('server','stop');
  assert.match(await cli('server','status'),/not running/);
  // Delay before exec of the identified agent. A sleeping process already
  // named codex would provide fresh process evidence to the owner.
  const real=join(home,'agent-real');await mkdir(real);await rename(join(home,'bin/codex'),join(real,'codex'));
  await writeFile(join(home,'fixture-wait'),'while [ -f "$MATRIX_HOLD_RESUME" ]; do sleep 0.1; done\nexec "$HOME/agent-real/codex" "$@"\n');
  await writeFile(join(home,'bin/codex'),'#!/bin/sh\nexec /bin/sh "$HOME/fixture-wait" "$@"\n',{mode:0o700});
  p=await launch();assert.notEqual((await snap()).server_generation,generation);
  const hist=async()=>(await api('automation.history',{id:before.id})).runs[0];
  await button(p,'Orchestration').click();await p.getByRole('tab',{name:'automations'}).click();
  await button(p.locator('.task-card').filter({hasText:'Durable check'}),'Run now…').click();await submit(p,'Run');
  await eventually('restore without readiness cannot deliver',async()=>(await hist())?.status==='pending');
  assert.ok(!JSON.stringify(await hist()).includes(nativeId));
  await unlink(env.MATRIX_HOLD_RESUME);
  await eventually('same exact session delivers after restart',async()=>(await hist())?.status==='delivered');
  const rebound=await definition();assert.equal(rebound.target.binding,'durable');assert.notEqual(rebound.target.terminal_id,old);
  const sid2=nativeId.slice(0,-1)+'8';
  await writeFile(join(env.CODEX_HOME,'sessions','2026','11','04','rollout-drift-'+sid2+'.jsonl'),JSON.stringify({type:'session_meta',payload:{id:sid2,cwd:workspace}})+'\n');
  await api('pane.close',{pane:String(rebound.target.pane_id)});
  await history(sid2);
  const other=await eventually('second agent detected',async()=>{
    const rows=(await api('terminal.backend.inventory')).terminals??[];
    return rows.find(t=>t.terminal_id!==rebound.target.terminal_id && t.terminal_title==='Codex' && String(t.pane_id)!==String(rebound.target.pane_id));
  });
  await button(p,'Orchestration').click();await p.getByRole('tab',{name:'automations'}).click();
  const card=p.locator('.task-card').filter({hasText:'Durable check'});
  await button(card,'Rebind terminal…').click();await choose(dialog(p).getByLabel('Agent terminal'),other.pane_id);
  await button(dialog(p),'Rebind').click();await dialog(p).getByRole('alert').filter({hasText:/identity|native|session|conversation|match/i}).waitFor();
  await button(dialog(p),'Cancel').click();
  assert.equal((await definition()).target.terminal_id,rebound.target.terminal_id);
  ok('restart parks delivery until fresh readiness; only the same native conversation can regain the binding');
});

await scenario('mixed-clients',async ({page:p,native,workspace,home,snap,cli,env,ok})=>{
  const original=await snap(), id=ids(original)[0];
  await p.locator('.session-selector').click();
  await button(p,'Manage connections').click();
  const web=p.locator('[data-bridge=web]');await button(web,'Start read-only').click();
  await native.evaluate(({clipboard})=>{clipboard.writeText=s=>{globalThis.fixturePairing=s;};});
  await button(web,'Copy pairing').click();
  const webReady=await native.evaluate(async({BrowserWindow})=>{
    const url=new URL(globalThis.fixturePairing);delete globalThis.fixturePairing;
    if(url.hostname!=='127.0.0.1')throw Error('expected loopback');
    const peer=new BrowserWindow({show:false,webPreferences:{partition:'test-readonly-web',sandbox:true,contextIsolation:true,nodeIntegration:false}});
    globalThis.fixtureWeb=peer;
    await peer.loadURL(url.origin+'/favicon.ico');
    return peer.webContents.executeJavaScript(`new Promise((resolve,reject)=>{
      const timer=setTimeout(()=>reject(Error('Web authorization timed out')),6000);
      const ws=window.fixtureSocket=new WebSocket('ws://'+location.host+'/bridge');
      ws.onopen=()=>ws.send(JSON.stringify({type:'authenticate',code:${JSON.stringify(decodeURIComponent(url.hash.slice(6)))}}));
      ws.onmessage=({data})=>{const msg=JSON.parse(data);if(msg.type==='ready'){clearTimeout(timer);resolve(msg.authority.mode)}else if(msg.type==='error'){clearTimeout(timer);reject(Error(msg.code))}};
    })`);
  });
  assert.equal(webReady,'read_only');
  await button(p,'Back to terminals').click();
  await p.locator('canvas').first().click();await p.keyboard.type("printf '%s%s\\n' MIXED_ BASELINE");await p.keyboard.press('Enter');
  await p.waitForFunction(()=>document.querySelector('.terminal-accessible')?.textContent.includes('MIXED_BASELINE'));
  const bin=join(home,'tui-bin');await mkdir(bin);await writeFile(join(bin,'pbcopy'),'#!/bin/sh\ncat > "$LUVUS_HOME/tui-clipboard"\n',{mode:0o700});
  const tui=spawn('python3',['-u',join(repo,'app/tests/tui-peer.py'),binary],{cwd:workspace,env:{...env,PATH:bin+':'+env.PATH},stdio:['pipe','pipe','inherit']});
  const closed=once(tui,'close');let painted=false,tail=false,notice=false;tui.stdout.on('data',s=>{painted||=String(s).includes('painted');tail||=String(s).includes('live-tail');notice||=String(s).includes('notice');});
  try {
    const send=cmd=>tui.stdin.write(JSON.stringify(cmd)+'\n');
    await eventually('TUI first paint',()=>painted);send({resize:[84,28]});
    send({input:"\x1b[200~printf '%s%s\\n' MIXED_ TUI; stty size > tui-size\x1b[201~\r"});
    const size=await eventually('TUI owns its PTY size',async()=>{const s=await readFile(join(workspace,'tui-size'),'utf8').catch(()=>'');return /^\d+ \d+$/.test(s.trim()) && s.trim();});
    await p.waitForFunction(()=>document.querySelector('.terminal-accessible')?.textContent.includes('MIXED_TUI'));
    await native.evaluate(({BrowserWindow})=>BrowserWindow.getAllWindows().find(w=>w.webContents.getURL().startsWith('luvus-app:')).setContentSize(900,600));
    await p.waitForFunction(()=>innerWidth===900);
    send({input:"\x1b[200~stty size > tui-size-after\x1b[201~\r"});
    await eventually('passive App resize leaves TUI PTY geometry unchanged',async()=>(await readFile(join(workspace,'tui-size-after'),'utf8').catch(()=>'')).trim()===size);
    const frame=()=>p.evaluate(async()=>(await window.luvus.bootstrap()).find(m=>m.type==='terminal')?.frame);
    send({input:"\x1b[200~i=0; while [ $i -lt 90 ]; do echo line-$i; i=$((i+1)); done\x1b[201~\r"});
    await eventually('scrollback exists',async()=>(await frame())?.history_rows>30);
    await p.locator('canvas').first().hover();await p.mouse.wheel(0,-600);
    await eventually('App privately scrolled',async()=>(await frame())?.scroll_offset>0);
    send({input:"\x1b[200~printf '%s%s\\n' LIVE_ TAIL_ONLY\x1b[201~\r"});
    await eventually('unscrolled TUI receives later live output',()=>tail);
    assert.ok((await frame()).scroll_offset>0);
    assert.ok(!(await p.locator('.terminal-accessible').first().textContent()).includes('LIVE_TAIL_ONLY'));
    await p.mouse.wheel(0,2000);await eventually('App returns to live tail',async()=>(await frame())?.scroll_offset===0);
    await native.evaluate(({clipboard})=>{clipboard.writeText=s=>{globalThis.privateCopy=s;};});
    const f=await frame();const r=f.cells.find(c=>c.text==='L' && c.col===0 && f.cells.filter(row=>row.row===c.row).map(c=>c.text).join('').includes('LIVE_TAIL_ONLY'))?.row;
    assert.notEqual(r,undefined);
    for(const input of [{kind:'selection',mode:'cell',geometry:f.geometry,revision:f.revision,start:[r,0],end:[r,13]},{kind:'copy',geometry:f.geometry}]){
      await p.evaluate(a=>window.luvus.request(a),{type:'input',id:crypto.randomUUID(),target:f.target,input});
    }
    await eventually('origin-only copy reached the App',async()=>await native.evaluate(()=>globalThis.privateCopy)==='LIVE_TAIL_ONLY');
    await cli('ui','notification','push','--text','MixedNotice','--level','info');
    assert.equal(await p.locator('.global-notice').count(),0);
    await eventually('TUI renders the shared notice',()=>notice);
    await assert.rejects(readFile(join(home,'tui-clipboard')),{code:'ENOENT'});
    const probe=await native.evaluate(async()=>globalThis.fixtureWeb.webContents.executeJavaScript(`new Promise((res,rej)=>{
      const ws=window.fixtureSocket;const timer=setTimeout(()=>rej(Error('web response timed out')),5000);
      ws.onmessage=({data})=>{const m=JSON.parse(data);if(m.type==='response'&&m.id==='mutate'){clearTimeout(timer);res(Boolean(m.error));}};
      ws.send(JSON.stringify({type:'request',id:'mutate',method:'workspace.new',params:{}}));
    })`));
    assert.equal(probe,true);assert.equal((await snap()).workspaces.length,original.workspaces.length);
    await p.locator('canvas').first().click();await p.keyboard.type("printf '%s%s\\n' MIXED_ APP");await p.keyboard.press('Enter');
    await eventually('App retakes input',async()=>JSON.parse(await cli('pane','read',id)).result.text.includes('MIXED_APP'));
    assert.equal((await snap()).server_generation,original.server_generation);
    ok('real TUI, App and paired Web share the owner: private scroll and copy, shared inbox notice, passive resize and read-only Web fences');
  } finally {
    tui.stdin.end();await closed;
    await native.evaluate(()=>{globalThis.fixtureWeb?.destroy();delete globalThis.fixtureWeb;});
  }
});

await scenario('slow-display',async ({page:p,home,workspace,env,ok})=>{
  await writeFile(join(workspace,'burst.py'),[
    'import sys,select,time,pathlib,os','w,h=os.get_terminal_size()','print("BURST_ARMED",flush=True)',
    'for i in range(200):','    if select.select([sys.stdin],[],[],0)[0]:',
    '        pathlib.Path("input-receipt").write_text(sys.stdin.readline().strip())',
    '    sys.stdout.write("\\033[H"+((chr(65+i%25)*(w-1)+"\\r\\n")*max(1,h-2)));sys.stdout.flush();time.sleep(0.025)',
    'print("\\033[2J\\033[HSLOW_FINAL",flush=True)'
  ].join('\n'));
  const peer=spawn(binary,['--session','matrix','client','--surface','app','--stdio'],{cwd:workspace,env,stdio:['pipe','pipe','inherit']});
  const closed=once(peer,'close');let ready=false,frames=0,tail=false,decodedError;
  const decoder=new ResponseDecoder(m=>{ready||=m.type==='ready';if(m.type==='terminal'){
    frames++;tail||=m.frame.cells.map(c=>c.text).join('').includes('SLOW_FINAL');
  }});
  peer.stdout.on('data',b=>{try{decoder.push(b);}catch(e){decodedError=e;}});
  try{
    await eventually('attached slow reader',()=>ready && frames>0);
    peer.stdout.pause();const pausedFrames=frames;
    await p.locator('canvas').first().click();await p.keyboard.type('python3 burst.py');await p.keyboard.press('Enter');
    await eventually('producer actually emitting output',async()=>{const s=await p.locator('.terminal-accessible').first().textContent();return /(A{12}|B{12}|C{12}|D{12}|E{12})/.test(s);});
    const then=performance.now();await p.keyboard.type('accepted-under-load');await p.keyboard.press('Enter');
    await eventually('owner accepts active input despite stopped reader',async()=>(await readFile(join(workspace,'input-receipt'),'utf8').catch(()=>''))==='accepted-under-load');
    const latency=Math.round(performance.now()-then);
    await p.waitForFunction(()=>document.querySelector('.terminal-accessible')?.textContent.includes('SLOW_FINAL'));
    assert.equal(frames,pausedFrames);
    peer.stdout.resume();await eventually('slow reader reaches current owner frame',()=>tail);
    assert.ok(!decodedError && frames-pausedFrames<100);
    await writeFile(join(home,'pressure.json'),JSON.stringify({producerUpdates:200,replayedFrames:frames-pausedFrames,inputMs:latency}));
    ok('200 forced paints, unconsumed stdio, active input received during production and slow reader converges to the final frame');
  }finally{peer.stdout.resume();peer.stdin.end(encodeRequest({type:'detach',id:'done'}));await closed;}
});

await scenario('active-automation',async ({page:p,home,workspace,snap,cli,env,ok})=>{
  const agent=join(home,'codex');await exec('/usr/bin/cc',[join(repo,'app/tests/fixtures/composer.c'),'-o',agent],{cwd:workspace,env});
  const id=ids(await snap())[0], canvas=()=>p.locator('canvas').first();
  await canvas().click();await p.keyboard.type('exec '+agent+' busy');await p.keyboard.press('Enter');
  await eventually('busy VT',async()=>(await p.locator('.terminal-accessible').first().textContent()).includes('Generating answer'));
  await p.locator('.agent-row').filter({hasText:/codex/i}).first().waitFor();
  const orch=async()=>{await button(p,'Orchestration').click();await p.getByRole('tab',{name:'automations'}).click();};
  await orch();
  const make=async(policy)=>{
    await button(p,'Schedule agent prompt…').click();const d=dialog(p);
    await d.getByLabel('Name',{exact:false}).fill('Fixture '+policy);
    await d.getByLabel('Prompt').fill('receipt-'+policy);
    await choose(d.getByLabel('Agent terminal'),id);await choose(d.getByLabel('When busy'),policy);
    await submit(p,'Schedule');
    const card=p.locator('.task-card').filter({has:p.getByRole('heading',{name:'Fixture '+policy})});
    await button(card,'Run now…').click();await submit(p,'Run');
    return JSON.parse(await cli('automation','list')).result.automations.find(a=>a.name==='Fixture '+policy).id;
  };
  const status=async key=>(JSON.parse(await cli('automation','history',key)).result.runs)[0];
  const skip=await make('skip');await eventually('skipped busy',async()=>(await status(skip))?.status==='skipped');
  const wait=await make('wait');await eventually('parked',async()=>{const run=await status(wait);return run?.status==='pending' && /waiting for target/.test(run.error);});
  await button(p,'Back to terminals').click();await canvas().click();await p.keyboard.type(':fixture-ready');await p.keyboard.press('Enter');
  await eventually('delivered',async()=>(await status(wait))?.status==='delivered');
  assert.ok(!(await status(wait)).task_id);
  const screen=await eventually('prompt consumed',async()=>{const s=await p.locator('.terminal-accessible').first().textContent();return s.includes('fixture accepted: receipt-wait') && s;});
  assert.doesNotMatch(screen,/receipt-skip/);assert.equal(JSON.parse(await cli('task','list')).result.tasks.length,0);
  ok('busy skip sends nothing; wait is queued until fresh readiness; delivered to the same terminal without creating a worker or marking work done');
});

await scenario('module-widgets',async ({page:p,home,ok})=>{
  const provider=join(home,'provider');await mkdir(provider);
  await writeFile(join(provider,'luvus-module.toml'),`id="fixture.widgets"
name="Matrix provider"
version="0.1.0"
min_luvus_version="1.0.0"
[[docks]]
id="rows"
title="Matrix dock"
placement="sidebar.left"
[[bars]]
id="status"
title="Matrix status"
region="top-right"
[[actions]]
id="fill"
title="Populate widgets"
command=["/bin/sh","provider.sh","fill"]
[[actions]]
id="clicked"
title="Record widget"
command=["/bin/sh","provider.sh","clicked"]
[[actions]]
id="fail"
title="Fixture failure"
command=["/bin/sh","provider.sh","fail"]
`);
  await writeFile(join(provider,'provider.sh'),`set -eu
case "$1" in
fill)
  "$LUVUS_BIN_PATH" ui dock push --id rows --title "Matrix dock" --rows '[{"text":"Action row","action":"clicked","value":"row-payload"}]'
  "$LUVUS_BIN_PATH" bar push --id status --region top-right --content '[{"type":"badge","text":"Widget button","action":"clicked","value":"bar-payload"}]'
  ;;
clicked) printf '%s%s' "\${LUVUS_MODULE_ROW_VALUE:-}" "\${LUVUS_MODULE_BAR_VALUE:-}" >>receipt ;;
fail) echo fixture-provider-error >&2;exit 2 ;;
esac
`);
  await button(p,'Modules').click();await button(p,'Link module…').click();
  await dialog(p).getByRole('textbox').fill(provider);await submit(p,'Link module');
  const owner=p.locator('.list-card').filter({hasText:'Matrix provider'});
  if (await button(owner,'Enable').isVisible()) await button(owner,'Enable').click();
  await owner.locator('.module-select').click();await button(p,'Populate widgets').click();await submit(p,'Run action');
  await button(p,'Action row').click();await button(p,'Widget button').click();
  await eventually('both widget receipts',async()=>{
    const receipt=await readFile(join(provider,'receipt'),'utf8').catch(()=>'');
    return receipt.includes('row-payload') && receipt.includes('bar-payload');
  });
  await button(p,'Fixture failure').click();await submit(p,'Run action');
  await p.getByRole('tab',{name:'logs',exact:true}).click();
  await eventually('failed provider log',async()=>{
    if((await p.locator('.tools-panel').innerText()).includes('fixture-provider-error'))return true;
    await button(p,'Refresh modules').click();return false;
  });
  ok('module-supplied row and bar dispatch their exact values; a failed action remains inspectable without stopping the core');
});

await scenario('diagnostics',async ({page:p,snap,ok})=>{
  const before=await snap();
  await p.locator('canvas').first().click();
  await p.keyboard.type('echo fixture-output-marker');await p.keyboard.press('Enter');
  await eventually('marker visible on owner before opening diagnostics',async()=>(await p.locator('.terminal-accessible').first().textContent()).includes('fixture-output-marker'));
  await p.locator('.session-selector').click();
  await button(p,'Manage connections').click();
  const listening=port=>new Promise(resolve=>{
    const socket=connect({host:'127.0.0.1',port});
    socket.once('connect',()=>{socket.destroy();resolve(true);});
    socket.once('error',()=>resolve(false));
  });
  for (const kind of ['web','uhp']) {
    const row=p.locator('[data-bridge='+kind+']');
    for(const control of [false,true]){
      if(control){await button(row,'Start with control…').click();await submit(p,'Start with control');}
      else await button(row,'Start read-only').click();
      const port=await eventually(kind+' port',async()=>Number((await row.textContent()).match(/127\.0\.0\.1:(\d+)/)?.[1])||null);
      assert.ok(await listening(port));assert.match(await row.innerText(),control?/control/:/read_only/);
      assert.doesNotMatch(await row.innerText(),/pair=|pairing_code|socket_path/);
      await button(row,'Stop bridge').click();
      await eventually(kind+' revoked',async()=>!await listening(port));
    }
    ok(kind+' read-only and confirmed control each bind loopback and revoke on stop');
  }
  await button(p,'Server log').click();await p.locator('[data-diagnostic]').getByText(/records/i).first().waitFor();
  const wait=async (marker,match)=>{
    await button(p,'Wait for output…').click();
    await dialog(p).getByLabel('Match text').fill(marker);await dialog(p).getByLabel('Timeout in seconds').fill('0.4');
    await submit(p,'Wait');
    await eventually('wait '+match,async()=>(await p.locator('[data-diagnostic]').innerText()).includes(match?'Yes':'No'));
  };
  await wait('fixture-output-marker',true);await wait('no-such-fixture-output',false);
  assert.equal((await snap()).server_generation,before.server_generation);
  ok('bounded source log headers, exact terminal match, timeout and unchanged owner generation');
});

await scenario('local-sessions',async ({page:p,native,cli,snap,launch,ok})=>{
  const original = await snap();
  const observer = await launch();
  const connection = async (name) => {
    await p.locator('.session-selector').filter({hasText:name}).click();
    await button(p,'Manage connections').click();
    await button(p,'List sessions').click();
  };
  await button(p,'Switch session').click();
  await button(p,'New session').click();
  await dialog(p).getByRole('textbox').fill('matrix-spare');
  await button(dialog(p),'Create and switch').click();
  await p.locator('.session-selector .session-name').filter({hasText:'matrix-spare'}).waitFor({timeout:25_000});
  assert.equal((await snap()).server_generation, original.server_generation);
  assert.equal(await observer.locator('.session-selector .session-name').textContent(),'matrix');
  ok('new session attaches only this window; original server and second client are unchanged');
  await button(p,'Switch session').click();
  await p.locator('.session-option').filter({has:p.getByText('matrix',{exact:true})}).click();
  const row = name=>p.locator('.list-card').filter({has:p.getByText(name,{exact:true})});
  await p.waitForFunction(()=>document.querySelector('.session-selector .session-name')?.textContent==='matrix',null,{timeout:25_000});
  await connection('matrix');
  // Drive the native confirmation boundary without touching system UI or
  // granting authority to the renderer to confirm its own requests.
  await native.evaluate(({dialog})=>{
    dialog.showMessageBox = async (_win,options) => {
      if (options.cancelId!==0 || options.defaultId!==0 || !options.message.includes('matrix-spare')) throw Error('unsafe lifecycle confirmation');
      const response = globalThis.matrixConfirm ?? 0; globalThis.matrixConfirm = 0;
      return {response,checkboxChecked:false};
    };
  });
  await button(row('matrix-spare'),'Stop…').click();
  await eventually('cancel leaves owner running',async()=>(await row('matrix-spare').innerText()).includes('Running'));
  await native.evaluate(()=>{globalThis.matrixConfirm=1;});
  await button(row('matrix-spare'),'Stop…').click();
  await button(row('matrix-spare'),'Delete…').waitFor();
  await native.evaluate(()=>{globalThis.matrixConfirm=1;});
  await button(row('matrix-spare'),'Delete…').click();
  await row('matrix-spare').waitFor({state:'detached'});
  assert.ok(!JSON.parse(await cli('session','list','--json')).sessions.some(s=>s.name==='matrix-spare'));
  assert.equal((await snap()).server_generation, original.server_generation);
  ok('cancel/stop/delete affect only the named inactive session; selected generation survives');
});

await scenario('commander-receipts',async ({page:p,workspace,home,snap,env,ok})=>{
  const fixture=join(home,'codex');
  await exec('/usr/bin/cc',[join(repo,'app/tests/fixtures/composer.c'),'-o',fixture],{cwd:workspace,env});
  const first=ids(await snap())[0];
  await button(p,'Split active pane').click(); assert.equal(await dialog(p).count(),0);
  await eventually('split',async()=>ids(await snap()).length===2);
  const other=ids(await snap()).find(id=>id!==first);
  await eventually('both split canvases painted',async()=>await p.locator('.pane canvas').count()===2);
  for (const [i,mode] of [[0,'idle'],[1,'busy']]) {
    const expected=ids(await snap())[i];
    const canvas=p.locator('.pane canvas').nth(i); await canvas.click({position:{x:10,y:10}});
    await eventually('owner focus before typing',async()=>tab(await snap()).panes.find(pane=>pane.focused)?.pane_id===expected);
    await p.keyboard.type('exec '+fixture+' '+mode); await p.keyboard.press('Enter');
    const sentinel=mode==='idle'?'Ready for your next task':'Generating answer (esc to cancel';
    await eventually('composer started '+mode,async()=>(await p.locator('.terminal-accessible').nth(i).textContent()).includes(sentinel));
  }
  await button(p,'Toggle Commander').click();
  const boxes=p.locator('.commander-targets input');
  await eventually('two detected fixture PTYs',async()=>await boxes.count()===2);
  await boxes.nth(0).check(); await boxes.nth(1).check();
  await p.getByRole('textbox',{name:'Commander prompt',exact:true}).fill('matrix-delivery');
  await button(p,'Review & send').click(); await submit(p,'Send to 2 agents');
  await eventually('one success one refusal',async()=>await p.locator('.receipts .muted').count()===1 && await p.locator('.receipts .destructive-text').count()===1);
  assert.equal(await p.locator('.commander-targets input:checked').count(),1);
  const live=await snap();
  assert.ok(ids(live).includes(first) && ids(live).includes(other));
  await eventually('idle pane consumed prompt',async()=>(await p.locator('.terminal-accessible').first().textContent()).includes('fixture accepted: matrix-delivery'));
  assert.doesNotMatch(await p.locator('.terminal-accessible').nth(1).textContent(),/matrix-delivery/);
  assert.equal(await p.getByRole('textbox',{name:'Commander prompt',exact:true}).inputValue(),'matrix-delivery');
  ok('independent strict readiness, owner delivery receipt and failed-only retry selection');
  await button(p,'Mission Control').click();
  await eventually('fleet rows',async()=>await p.locator('.fleet-row').count()>=2);
  await button(p.locator('.fleet-row').first(),'Focus').click();
  assert.ok(ids(await snap()).includes(first));
  ok('Mission Control lists the detected PTYs without fabricating token counts');
});

if (!Object.keys(report.cases).length) throw new Error('no matching matrix case');
if (Object.values(report.cases).some(c=>!c.pass)) process.exitCode=1;
