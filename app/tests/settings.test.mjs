import assert from 'node:assert/strict';
import { test } from 'node:test';
import { readFile } from 'node:fs/promises';
import { build } from 'esbuild';
import vm from 'node:vm';

const output=await build({entryPoints:[new URL('../apps/renderer/src/settings-schema.ts',import.meta.url).pathname],bundle:true,format:'cjs',write:false});
const context={module:{exports:{}}};vm.runInNewContext(output.outputFiles[0].text,context);
const {settingSpecs,settingValue,numericSetting}=context.module.exports;
const copy=JSON.parse(await readFile(new URL('../apps/renderer/src/settings-copy.json',import.meta.url),'utf8'));
test('settings catalog is explicit, localized, and excludes storage metadata',()=>{
  assert.equal(new Set(settingSpecs.map(s=>s.path)).size,settingSpecs.length);
  for(const spec of settingSpecs) {
    assert.ok(copy[spec.title],spec.title);
    assert.ok(copy[spec.group],spec.group);
    if(spec.kind==='choice')for(const label of Object.values(spec.options))assert.ok(copy[label] || ['Light','Dark','Follow system','ESC LF','CSI-u'].includes(label),label);
    if(spec.kind==='number')assert.ok(spec.min>=0 && spec.max>=spec.min);
  }
  assert.ok(!settingSpecs.some(s=>['version','sidebar_width','layout.scrollback','keybindings','direct_keybindings','mission_pricing','remote_hosts','bars.off'].includes(s.path)));
});
test('numeric controls validate before submission and convert MiB to bytes',()=>{
  const spec=settingSpecs.find(s=>s.path==='layout.scrollback_bytes');
  assert.equal(numericSetting('32',spec),32*1024*1024);
  for(const invalid of ['', ' ', '0','257','1.5','NaN','Infinity'])assert.throws(()=>numericSetting(invalid,spec));
  const width=settingSpecs.find(s=>s.path==='sidebars.left.width');
  assert.throws(()=>numericSetting('17',width));assert.throws(()=>numericSetting('45',width));
  assert.equal(settingValue({layout:{diff_layout:'split'}},'layout.diff_layout'),'split');
  assert.equal(settingValue({layout:null},'layout.diff_layout'),undefined);
});
