import assert from 'node:assert/strict';
import { test } from 'node:test';
import { build } from 'esbuild';
import vm from 'node:vm';
const output=await build({entryPoints:[new URL('../apps/renderer/src/keyboard.ts',import.meta.url).pathname],bundle:true,format:'cjs',write:false});
const context={module:{exports:{}}};vm.runInNewContext(output.outputFiles[0].text,context);
const {commandKey,matchesChord,tabDigit}=context.module.exports;
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
