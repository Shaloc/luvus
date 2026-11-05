import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
const exec = promisify(execFile);

export function validName(name: unknown): name is string {
  return typeof name === 'string' && /^[a-zA-Z0-9._-]{1,64}$/.test(name) && !name.startsWith('-') && name !== '.' && name !== '..';
}
export interface Inventory {
  sessions: {name:string; running:boolean}[];
  hosts: {host:string; error:string|null}[];
  merge:boolean;
}
export class SessionAdmin {
  private readonly env:NodeJS.ProcessEnv;
  constructor(private readonly binary:string, private readonly selected:string, environment:NodeJS.ProcessEnv) {
    this.env = {...environment};
    for (const name of ['LUVUS_SOCKET_PATH','LUVUS_SESSION','LUVUS_REMOTE_HOST','LUVUS_REMOTE_SESSION','LUVUS_PANE_ID','LUVUS_API_ADDRESS']) delete this.env[name];
  }
  private async cli(args:string[]) {
    const {stdout} = await exec(this.binary,['--session',this.selected,...args],{env:this.env,timeout:35_000,maxBuffer:2*1024*1024});
    return JSON.parse(stdout);
  }
  async inventory():Promise<Inventory> {
    const raw = await this.cli(['session','list','--json']);
    if (!Array.isArray(raw.sessions) || !Array.isArray(raw.hosts) || !Array.isArray(raw.remote_sessions)) throw new Error('Invalid session inventory');
    const remotes = new Set(raw.remote_sessions.map((r:{host:string;session:string})=>'remote-'+r.host+'-'+r.session));
    // Do not expose endpoint paths or confuse an SSH presentation with a local owner.
    return {sessions:raw.sessions.filter((s:{name:string})=>validName(s.name) && !remotes.has(s.name))
      .map((s:{name:string;running:boolean})=>({name:s.name,running:s.running===true})),
      hosts:raw.hosts.map((h:{host:string;error:string|null})=>({host:h.host,error:h.error})),merge:raw.merge===true};
  }
  async check(name:string, create:boolean) {
    if (!validName(name) || name.startsWith('remote-')) throw new Error('Choose a local session name');
    const inventory = await this.inventory();
    const row = inventory.sessions.find(s=>s.name === name);
    if (create ? row : !row) throw new Error(create ? 'Session already exists' : 'Session no longer exists');
    return row;
  }
  async lifecycle(operation:'stop'|'delete', name:string) {
    if (name===this.selected || operation==='delete' && name==='default') throw new Error('This session cannot be changed here');
    const row = await this.check(name,false);
    if (!row || row.running !== (operation==='stop')) throw new Error('Session state changed; refresh the inventory');
    await this.cli(['session',operation,name,'--json']);
  }
  async addHost(alias:string) {
    if (!/^[a-zA-Z0-9][a-zA-Z0-9_.-]{0,253}$/.test(alias)) throw new Error('Choose a literal SSH Host alias');
    // The core validates ~/.ssh/config and version. This operation grants no
    // install/update authority and cannot restart an existing owner.
    await this.cli(['host','add',alias,'--json']);
  }
  async setMerge(enabled:boolean) {
    await this.cli(['session','merge',enabled?'on':'off','--json']);
  }
  private async addonRequest(method:string,params:Record<string,unknown>={}) {
    const run=exec(this.binary,['--session',this.selected,'uhp','proxy'],{env:this.env,timeout:12_000,maxBuffer:512*1024});
    run.child.stdin?.end(JSON.stringify({id:'desktop-addons',method,params})+'\n');
    const {stdout}=await run.catch((e:unknown)=>{
      if(e && typeof e==='object' && 'stdout' in e && typeof e.stdout==='string' && e.stdout)return {stdout:e.stdout};
      throw Error('Local add-on command did not complete');
    });
    const reply=JSON.parse(stdout);
    if(reply.error || !reply.result)throw Error(typeof reply.error?.code==='string'?reply.error.code:'Invalid add-on response');
    return reply.result;
  }
  async addons() {
    const integration=await this.addonRequest('integration.status');
    const skill=await this.addonRequest('skill.status');
    if(!Array.isArray(integration.integrations)||!Array.isArray(skill.destinations))throw Error('Invalid local add-on inventory');
    return {integrations:integration.integrations.map((v:{agent:string;installed:boolean})=>({agent:v.agent,installed:v.installed===true})),
      skills:skill.destinations.map((v:{host:string;state:string})=>({host:v.host,state:v.state})),release:skill.bundled_release};
  }
  async changeAddon(op:'install'|'uninstall'|'enable'|'disable',agent:string){
    if(op==='install'||op==='uninstall'){
      const {integrations}=await this.addons();
      if(!integrations.some((v:{agent:string})=>v.agent===agent))throw Error('Unknown integration');
      await this.addonRequest('integration.'+op,{agent,confirm:true});
    }else await this.addonRequest('skill.'+op,{confirm:true});
    return this.addons();
  }
}
