import assert from 'node:assert/strict';
import {EventEmitter} from 'node:events';
import {mkdtemp,readdir} from 'node:fs/promises';
import {join} from 'node:path';
import {fileURLToPath} from 'node:url';
import test from 'node:test';
import {build} from 'esbuild';

test('revocation removes display staging even if the owner never acknowledges the download', {timeout:2500},async()=>{
  const root=fileURLToPath(new URL('../../target/',import.meta.url)),home=await mkdtemp(join(root,'app-e2e-revoke-'));
  const old=process.env.TMPDIR;process.env.TMPDIR=home;
  try{
    const built=await build({entryPoints:[fileURLToPath(new URL('../apps/desktop/src/browser-downloads.ts',import.meta.url))],bundle:true,write:false,platform:'node',format:'esm'});
    const {BrowserDownloads}=await import('data:text/javascript;base64,'+Buffer.from(built.outputFiles[0].contents).toString('base64'));
    let finish,released=0;
    const dl=new BrowserDownloads(()=>{},()=>new Promise(r=>{finish=r;}),()=>assert.fail('revocation is not a provider failure'),{reserve:()=>({release(){released++;}})});
    await dl.configure('fixture');
    const item=Object.assign(new EventEmitter(),{getTotalBytes:()=>9,getURL:()=>'https://fixture.invalid/file',getFilename:()=>'file',
      setSavePath(path){assert.ok(path.startsWith(home+'/'));},cancel(){this.cancelled=true;setTimeout(()=>this.emit('done',{},'cancelled'),20);}});
    assert.ok(dl.accept(item,'frame'));assert.equal(typeof finish,'function');
    await dl.dispose();assert.deepEqual(await readdir(home),[]);assert.ok(item.cancelled);assert.equal(released,1);
    finish();await dl.dispose();assert.deepEqual(await readdir(home),[]);
  }finally{if(old===undefined)delete process.env.TMPDIR;else process.env.TMPDIR=old;}
});
