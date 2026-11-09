import assert from 'node:assert/strict';
import { test } from 'node:test';
import { build } from 'esbuild';
import vm from 'node:vm';
const output=await build({entryPoints:[new URL('../apps/renderer/src/keyboard.ts',import.meta.url).pathname],bundle:true,format:'cjs',write:false});
const context={module:{exports:{}}};vm.runInNewContext(output.outputFiles[0].text,context);
const {commandKey,matchesChord,tabDigit,recordedBinding,bindingPatch}=context.module.exports;
const key=(key,mods={})=>({key,ctrlKey:false,altKey:false,shiftKey:false,metaKey:false,...mods});
test('prefix modifiers are exact, with core special and shifted suffix names',()=>{
  assert.ok(matchesChord(key('b',{ctrlKey:true}),'ctrl+b'));
  assert.ok(matchesChord(key(' ',{ctrlKey:true}),'ctrl+space'));
  assert.ok(matchesChord(key('F6',{shiftKey:true}),'shift+f6'));
  assert.ok(!matchesChord(key('b',{ctrlKey:true,metaKey:true}),'ctrl+b'));
  assert.ok(!matchesChord(key('b',{ctrlKey:true,shiftKey:true}),'ctrl+b'));
  assert.equal(commandKey(key('Tab',{shiftKey:true})),'⇧⇥');
  assert.equal(commandKey(key('ArrowLeft')),'←');
  assert.equal(commandKey(key('1',{shiftKey:true})),'!');
  assert.equal(commandKey(key('!',{shiftKey:true})),'!');
  assert.equal(commandKey(key('Enter')),'Enter');
});
test('only unshifted Command 1 through 9 selects positional tabs',()=>{
  for(let i=1;i<=9;i++)assert.equal(tabDigit(key(String(i),{metaKey:true})),i);
  for(const input of [key('0',{metaKey:true}),key('1'),key('1',{metaKey:true,shiftKey:true}),key('1',{metaKey:true,ctrlKey:true})]) assert.equal(tabDigit(input),undefined);
});
test('recorded shortcuts stay within each core key parser domain',()=>{
  for(const input of ['Backspace','Delete','Home','F1','Dead','Unidentified'])assert.equal(recordedBinding(key(input),'key'),undefined);
  assert.equal(recordedBinding(key('Tab',{shiftKey:true}),'key'),'⇧⇥');
  assert.equal(recordedBinding(key('ArrowLeft'),'key'),'←');
  assert.equal(recordedBinding(key('F1'),'direct'),undefined);
  assert.equal(recordedBinding(key('F1'),'prefix'),'f1');
  assert.equal(recordedBinding(key('F12',{shiftKey:true}),'prefix'),'shift+f12');
  assert.equal(recordedBinding(key('ArrowLeft',{ctrlKey:true}),'prefix'),undefined);
  assert.equal(recordedBinding(key('a',{ctrlKey:true,shiftKey:true}),'prefix'),undefined);
  assert.equal(recordedBinding(key('ArrowLeft',{shiftKey:true}),'direct'),'shift+left');
  assert.equal(recordedBinding(key('F2',{shiftKey:true}),'direct'),'shift+f2');
  assert.equal(recordedBinding(key('Escape',{ctrlKey:true}),'direct'),'ctrl+escape');
  assert.equal(recordedBinding(key('Backspace',{ctrlKey:true}),'direct'),undefined);
  assert.equal(recordedBinding(key('a',{shiftKey:true}),'direct'),undefined);
  assert.equal(recordedBinding(key('a',{metaKey:true}),'direct'),undefined);
});
test('rebinding atomically clears colliding explicit overrides but preserves other commands',()=>{
  assert.equal(JSON.stringify(bindingPatch({new_tab:'Z',close_tab:'X'},'focus_left','Z',false)),JSON.stringify({new_tab:'',focus_left:'Z'}));
  assert.equal(JSON.stringify(bindingPatch({new_tab:'Alt+Shift+N',close_tab:'ctrl+x'},'focus_left','alt+shift+n',true)),JSON.stringify({new_tab:'',focus_left:'alt+shift+n'}));
  assert.equal(JSON.stringify(bindingPatch({new_tab:'option+control+page-up'},'focus_left','ctrl+alt+pageup',true)),JSON.stringify({new_tab:'',focus_left:'ctrl+alt+pageup'}));
});
