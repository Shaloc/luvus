import assert from 'node:assert/strict';
import { execFile, spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { createInterface } from 'node:readline';
import { promisify } from 'node:util';
const exec=promisify(execFile);

export async function observeAgentBrowser({tool,binary,browserId,env,home,workspace}) {
  const toolEnv={...env,HOME:home,AGENT_BROWSER_NO_VERSION_CHECK:'1',AGENT_BROWSER_AUTOSAVE_INTERVAL_MS:'0'};
  for (const key of Object.keys(toolEnv)) if (key.startsWith('AGENT_BROWSER_') && !['AGENT_BROWSER_NO_VERSION_CHECK','AGENT_BROWSER_AUTOSAVE_INTERVAL_MS'].includes(key)) delete toolEnv[key];
  for (const key of Object.keys(toolEnv)) if (/^(http_proxy|https_proxy|all_proxy|no_proxy)$/i.test(key)) delete toolEnv[key];
  toolEnv.AGENT_BROWSER_SOCKET_DIR=join(home,'ab');
  await mkdir(toolEnv.AGENT_BROWSER_SOCKET_DIR,{mode:0o700});
  assert.match((await exec(tool,['--version'],{env:toolEnv})).stdout,/\b0\.41\.2\b/);
  const gateway=spawn(binary,['--session',env.LUVUS_SESSION,'browser','cdp','--browser',browserId,'--ttl','120','--json'],{cwd:workspace,env,stdio:['ignore','pipe','inherit']});
  const exited=once(gateway,'exit'), lines=createInterface({input:gateway.stdout});
  const base=['--session','x','--namespace','t','--json'];
  let endpoint, connected=false;
  try {
    const [line]=await Promise.race([once(lines,'line',{signal:AbortSignal.timeout(15_000)}),exited.then(()=>{throw Error('gateway exited early');})]);
    endpoint=JSON.parse(line).endpoint;
    const call=async (...args)=>{
      try {
        const result=JSON.parse((await exec(tool,[...base,...args],{cwd:workspace,env:toolEnv,timeout:20_000,maxBuffer:2*1024*1024})).stdout);
        assert.ok(result.success, 'agent-browser rejected '+args.at(-1));
        return result.data;
      } catch(e) {throw new Error('agent-browser: '+String(e.stdout || e.stderr || e.message).replaceAll(endpoint,'[private endpoint]').slice(0,800));}
    };
    connected=true;
    const inventory=await call('--cdp',endpoint,'snapshot','-i');
    assert.match(JSON.stringify(inventory),/Greet/);
    await call('fill','#name','Single authorized tab');
    await call('click','#greet');
    assert.match(JSON.stringify(await call('get','text','#result')),/Hello Single authorized tab/);
    const png=join(home,'agent-browser.png');
    await call('screenshot',png);
    assert.ok((await readFile(png)).subarray(0,8).equals(Buffer.from('89504e470d0a1a0a','hex')));
    await writeFile(join(home,'agent-browser.json'),JSON.stringify({version:'0.41.2',snapshot:true,click:true,ownerScreenshot:true}));
    console.error('PASS: agent-browser 0.41.2 connects to one authorized tab, snapshots, fills, clicks and captures it');
  } finally {
    if (connected) await exec(tool,[...base,'close'],{cwd:workspace,env:toolEnv,timeout:8000}).catch(()=>{});
    lines.close();
    if (gateway.exitCode===null && gateway.signalCode===null) gateway.kill('SIGTERM');
    await exited;
  }
}
