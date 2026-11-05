import type { Request,Target,TerminalFrame } from '@luvus/app-protocol';

type Tiles=NonNullable<TerminalFrame['graphics']>;
const MAX_IMAGE=8*1024*1024, POOL=32*1024*1024;
let reserved=0;
const waiting=new Set<()=>void>();
const wake=()=>{for(const fn of [...waiting])fn();};
type Entry={state:'waiting'|'loading'|'ready'|'failed';bitmap?:ImageBitmap;bytes:number};

async function inflate(bytes:Uint8Array<ArrayBuffer>, expected:number) {
  const reader=new Blob([bytes]).stream().pipeThrough(new DecompressionStream('deflate')).getReader();
  const out=new Uint8Array(expected);let offset=0;
  try {
    for(;;){const {done,value}=await reader.read();if(done)break;
      if(value.byteLength>expected-offset)throw Error('Oversized terminal image');out.set(value,offset);offset+=value.byteLength;
    }
  } finally {await reader.cancel().catch(()=>{});}
  if(offset!==expected)throw Error('Incomplete terminal image');return out;
}
async function decode(value:unknown,sequence:string):Promise<ImageBitmap> {
  const image=value as Record<string,unknown>,w=image.width,h=image.height,f=image.format;
  if(image.sequence!==sequence || typeof w!=='number' || typeof h!=='number' || !Number.isSafeInteger(w) || !Number.isSafeInteger(h)
      || w<1 || h<1 || w*h>MAX_IMAGE/4 || typeof image.data!=='string' || image.data.length>4*1024*1024)throw Error('Invalid terminal image');
  const bytes=Uint8Array.from(atob(image.data),ch=>ch.charCodeAt(0));
  if(f===100) {
    // Check the real PNG dimensions before passing untrusted bytes to Chromium.
    const dv=new DataView(bytes.buffer);
    if(image.compressed || bytes.length<24 || dv.getUint32(0)!==0x89504e47 || dv.getUint32(4)!==0x0d0a1a0a ||
       dv.getUint32(12)!==0x49484452 || dv.getUint32(16)!==w || dv.getUint32(20)!==h)throw Error('Invalid terminal PNG');
    return createImageBitmap(new Blob([bytes],{type:'image/png'}));
  }
  if(f!==24 && f!==32)throw Error('Unsupported terminal image');
  const channels=f/8,expected=w*h*channels;
  const raw=image.compressed?await inflate(bytes,expected):bytes;
  if(raw.length!==expected)throw Error('Invalid pixel byte count');
  const rgba=new Uint8ClampedArray(w*h*4);
  if(channels===4)rgba.set(raw); else {
    for(let i=0,j=0;i<raw.length;i+=3,j+=4){rgba[j]=raw[i]!;rgba[j+1]=raw[i+1]!;rgba[j+2]=raw[i+2]!;rgba[j+3]=255;}
  }
  return createImageBitmap(new ImageData(rgba,w,h));
}

/** The bitmap budget is shared by all panes in this renderer. A disposed or
 * replaced async decode can never install pixels into a different lifetime. */
export class TerminalImages {
  private entries=new Map<string,Entry>();
  private layers:Tiles=[];
  constructor(private target:Target,private send:(r:Request)=>Promise<unknown>,private paint:()=>void) {}
  update(layers:Tiles) {
    this.layers=layers.slice(0,128);
    const ids=new Set(this.layers.map(g=>g.sequence));
    for(const [id,e] of this.entries)if(!ids.has(id)){
      this.entries.delete(id);if(e.bitmap){reserved-=e.bytes;e.bitmap.close();}
    }
    for(const id of ids)if(!this.entries.has(id))this.entries.set(id,{state:'waiting',bytes:0});
    this.pump();wake();
  }
  private pump=()=>{
    waiting.delete(this.pump);
    for(const [id,e] of this.entries)if(e.state==='waiting'){
      if(reserved+MAX_IMAGE>POOL){waiting.add(this.pump);return;}
      e.state='loading';reserved+=MAX_IMAGE;
      void this.send({type:'graphic',id:crypto.randomUUID(),target:this.target,sequence:id}).then(r=>decode(r,id)).then(bitmap=>{
        if(this.entries.get(id)!==e){bitmap.close();return;}
        e.bitmap=bitmap;e.bytes=bitmap.width*bitmap.height*4;e.state='ready';reserved+=e.bytes;this.paint();
      }).catch(()=>{e.state='failed';}).finally(()=>{reserved-=MAX_IMAGE;wake();});
    }
  };
  draw(ctx:CanvasRenderingContext2D,cw:number,ch:number,frame:TerminalFrame){
    let count=0;
    for(const layer of this.layers){
      const bitmap=this.entries.get(layer.sequence)?.bitmap;if(!bitmap)continue;
      const sw=bitmap.width/layer.cols,sh=bitmap.height/layer.rows;
      for(const [x,y,sx,sy] of layer.cells){
        if(++count>frame.cols*frame.rows)return;
        if(x<frame.cols && y<frame.rows && sx<layer.cols && sy<layer.rows)ctx.drawImage(bitmap,sx*sw,sy*sh,sw,sh,x*cw,y*ch,cw,ch);
      }
    }
  }
  dispose(){this.update([]);waiting.delete(this.pump);}
}
