import assert from 'node:assert/strict';
import {test} from 'node:test';
import {build} from 'esbuild';
const result=await build({entryPoints:[new URL('../apps/desktop/src/core-client.ts',import.meta.url).pathname],bundle:true,format:'esm',platform:'node',write:false});
const {CoreClient}=await import(`data:text/javascript;base64,${Buffer.from(result.outputFiles[0].text).toString('base64')}`);
const fixture=()=>{
  const client=new CoreClient('/unused','fixture',{});
  client.child={stdin:{write(){}}};
  return client;
};
test('snapshot refresh waits for the frame following acknowledgement and publishes before resolving',async()=>{
  const client=fixture(),order=[];
  client.on('message',()=>order.push('frame'));
  const request=client.request({type:'snapshot',id:'refresh'}).then(value=>{order.push('done');return value;});
  client.receive({type:'snapshot',snapshot:{event_sequence:'1'}});
  client.receive({type:'reply',id:'refresh',result:{queued:true}});
  await Promise.resolve();assert.deepEqual(order,['frame']);
  client.receive({type:'snapshot',snapshot:{event_sequence:'2'}});
  assert.deepEqual(await request,{event_sequence:'2'});
  assert.deepEqual(order,['frame','frame','done']);assert.equal(client.pending.size,0);
});
test('snapshot wait fails closed on connection loss and ordinary receipts are unchanged',async()=>{
  const client=fixture();
  const receipt=client.request({type:'action',id:'receipt',generation:'fixture',method:'tab.new',params:{}});
  client.receive({type:'reply',id:'receipt',result:{ok:true}});
  assert.deepEqual(await receipt,{ok:true});
  const ordinary=client.request({type:'snapshot',id:'bad'});
  client.receive({type:'error',id:'bad',code:'invalid_request',message:'invalid'});
  await assert.rejects(ordinary,/invalid/);
  const pending=client.request({type:'snapshot',id:'waiting'});
  client.receive({type:'reply',id:'waiting',result:{queued:true}});
  client.fail(new Error('connection lost'));
  await assert.rejects(pending,/connection lost/);assert.equal(client.pending.size,0);
});
