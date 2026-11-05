import assert from 'node:assert/strict';
import {execFile} from 'node:child_process';
import {promisify} from 'node:util';
import {createSocket} from 'node:dgram';
import {createServer as http} from 'node:http';
import {createServer as https} from 'node:https';
import {readFile,writeFile,unlink} from 'node:fs/promises';
import {join} from 'node:path';

export async function observeLocalNetwork(desktop,home){
  const listen=(s,addr)=>new Promise((ok,fail)=>{s.once('error',fail);s.listen(0,addr,()=>ok(s.address().port));});
  const close=s=>new Promise(r=>{s.closeAllConnections();s.close(r);});
  const guest=await desktop.evaluate(({BrowserWindow})=>BrowserWindow.getAllWindows()[0].contentView.children.find(c=>c.webContents?.getURL().startsWith('http'))?.webContents.id);
  assert.ok(guest);
  const v6=http((_req,res)=>res.end('<title>IPv6 fixture</title>'));
  const key=join(home,'tls-key.pem'),cert=join(home,'tls-cert.pem'),log=join(home,'tls.netlog');
  await promisify(execFile)('/usr/bin/openssl',['req','-x509','-newkey','rsa:2048','-nodes','-days','1','-subj','/CN=localhost','-keyout',key,'-out',cert],{timeout:10_000});
  const pem=await readFile(cert),leaf=pem.toString().replace(/\s/g,'');
  let datagrams=0,hits=0;
  const udp=createSocket('udp4');udp.on('message',()=>datagrams++);await new Promise(r=>udp.bind(0,'127.0.0.1',r));
  const tls=https({cert:pem,key:await readFile(key)},(_req,res)=>{
    hits++;res.writeHead(200,{'alt-svc':'h3=":'+udp.address().port+'"; ma=120','cache-control':'no-store','content-type':'text/html'});
    res.end('<title>Pinned TLS fixture</title><body>from secure endpoint</body>');
  });
  let logging=false;
  try{
    const six=await listen(v6,'::1'),secure=await listen(tls,'127.0.0.1');
    const navigate=url=>desktop.evaluate(async({webContents},{guest,url})=>{
      const w=webContents.fromId(guest);await w.loadURL(url);return w.getTitle();
    },{guest,url});
    assert.equal(await navigate('http://[::1]:'+six+'/'),'IPv6 fixture');
    await assert.rejects(navigate('https://127.0.0.1:'+secure+'/'),/CERT_/);
    // Trust only the generated leaf in this one test guest. Everything else
    // uses Chromium verification. No system trust changes or ignore-TLS flags.
    await desktop.evaluate(async({webContents},{guest,leaf,log})=>{
      const ses=webContents.fromId(guest).session;
      ses.setCertificateVerifyProc((r,cb)=>cb(r.hostname==='localhost' && r.certificate.data.replace(/\s/g,'')===leaf?0:-3));
      await ses.netLog.startLogging(log);
    },{guest,leaf,log});logging=true;
    assert.equal(await navigate('https://localhost:'+secure+'/'),'Pinned TLS fixture');
    const permissions=await desktop.evaluate(({webContents},id)=>webContents.fromId(id).executeJavaScript(`(async()=>{
      if(!isSecureContext)throw Error('permission probe requires a secure page');
      const pairs=await Promise.all(['camera','microphone','clipboard-read','geolocation'].map(async name=>[name,(await navigator.permissions.query({name})).state]));
      return Object.fromEntries(pairs);
    })()`),guest);
    assert.ok(Object.values(permissions).every(s=>s==='denied'||s==='prompt'),'a secure page should have no pre-granted permission');
    const text=await desktop.evaluate(({webContents},id)=>webContents.fromId(id).executeJavaScript("fetch('/again',{cache:'no-store'}).then(r=>r.text())"),guest);
    assert.match(text,/from secure endpoint/);assert.ok(hits>=2);
    await desktop.evaluate(({webContents},id)=>webContents.fromId(id).session.netLog.stopLogging(),guest);logging=false;
    const net=JSON.parse(await readFile(log,'utf8'));
    assert.ok(net.events.some(e=>JSON.stringify(e.params??{}).toLowerCase().includes('alt-svc')));
    const quic=net.events.filter(e=>e.type===net.constants.logEventTypes.QUIC_SESSION);
    assert.equal(quic.length,0);assert.equal(datagrams,0);
    await writeFile(join(home,'network.json'),JSON.stringify({ipv6:true,rejectedUntrustedTLS:true,pinnedHTTPSRequests:hits,observedAltSvc:true,quicSessions:quic.length,displayUdpPackets:datagrams,permissions}));
  }finally{
    await desktop.evaluate(async({webContents},{guest,logging})=>{const ses=webContents.fromId(guest).session;if(logging)await ses.netLog.stopLogging();ses.setCertificateVerifyProc(null);},{guest,logging});
    if(v6.listening)await close(v6);if(tls.listening)await close(tls);udp.close();
    await unlink(key);await unlink(log).catch(()=>{});
  }
}
