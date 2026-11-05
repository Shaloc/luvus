import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import { clipboard } from 'electron';

type Kind='web'|'uhp';
interface Managed {
  child:ChildProcessWithoutNullStreams;closed:Promise<void>;mode:'read_only'|'control';
  pairing?:string;port?:number;
}
/** Owns only explicitly launched children of this window. Handing out a
 * pairing code or shutting down a bridge never exposes the owner socket. */
export class Gateways {
  private children=new Map<Kind,Managed>();
  constructor(private binary:string,private session:string,private env:NodeJS.ProcessEnv) {}
  status(){
    return (['web','uhp'] as const).map(kind=>{
      const row=this.children.get(kind);
      return {kind,state:row?.port?'ready':row?'starting':'stopped',port:row?.port??null,mode:row?.mode??null};
    });
  }
  async start(kind:Kind,control:boolean) {
    if(this.children.has(kind))throw Error('Bridge is already running');
    const argv=kind==='web'?['web','--port','0','--no-open']:['uhp','access','--ttl','600'];
    if(control)argv.push('--control');
    const env={...this.env};for(const k of ['LUVUS_SOCKET_PATH','LUVUS_SESSION','LUVUS_PANE_ID','LUVUS_REMOTE_HOST','LUVUS_REMOTE_SESSION','LUVUS_API_ADDRESS'])delete env[k];
    const child=spawn(this.binary,['--session',this.session,...argv],{env,stdio:'pipe',windowsHide:true});
    const closed=new Promise<void>(r=>child.once('close',()=>r()));
    const row:Managed={child,closed,mode:control?'control':'read_only'};this.children.set(kind,row);
    void closed.then(()=>{row.pairing=undefined;if(this.children.get(kind)===row)this.children.delete(kind);});
    child.stderr.resume();
    try {
      await new Promise<void>((res,rej)=>{
        let buf='',bytes=0;
        const finish=(e?:Error)=>{clearTimeout(timer);child.stdout.off('data',read);child.stdout.resume();child.off('close',early);child.off('error',finish);if(e)rej(e);else res();};
        const early=()=>finish(new Error('Bridge exited before it was ready'));
        const timer=setTimeout(()=>finish(new Error('Bridge startup timed out')),15_000);
        const read=(data:Buffer)=>{
          bytes+=data.length;if(bytes>64*1024){finish(new Error('Invalid bridge descriptor'));return;}
          buf+=data.toString('utf8');
          try {
            for(let end;(end=buf.indexOf('\n'))>=0;){
              const line=buf.slice(0,end).trim();buf=buf.slice(end+1);
              if(kind==='web'){
                if(!line.startsWith('http://'))continue;
                const url=new URL(line);
                if(url.hostname!=='127.0.0.1' || !url.hash.startsWith('#pair=') || !url.port)throw Error('Invalid loopback pairing URL');
                row.pairing=url.href;row.port=Number(url.port);
              }else{
                const info=JSON.parse(line);
                if(info.type!=='luvus_uhp_access'||info.endpoint?.host!=='127.0.0.1'||!Number.isInteger(info.endpoint.port) ||
                    info.endpoint.port<1 || info.endpoint.port>65535 || info.authority?.mode!==row.mode || typeof info.pairing?.code!=='string')throw Error('Invalid private access descriptor');
                row.pairing=JSON.stringify(info);row.port=info.endpoint.port;
              }
              finish();return;
            }
          }catch{finish(new Error('Invalid bridge descriptor'));}
        };
        child.stdout.on('data',read);child.once('close',early);child.once('error',finish);
      });
    }catch(e){await this.stop(kind);throw e;}
    return this.status();
  }
  copy(kind:Kind){
    const row=this.children.get(kind);
    if(!row?.pairing)throw Error('Start the bridge before copying its one-use pairing');
    clipboard.writeText(row.pairing);
  }
  async stop(kind:Kind){
    const row=this.children.get(kind);
    if(row){row.pairing=undefined;row.child.kill('SIGINT');
      const timer=setTimeout(()=>row.child.kill('SIGKILL'),4000);
      try{await row.closed;}finally{clearTimeout(timer);}}
    return this.status();
  }
  async close(){await Promise.all([...this.children.keys()].map(k=>this.stop(k)));}
}
