// Native SNS MP4s encrypt only their first 128 KiB. Keep the rest streaming.
const {once} = require('events');
const {initWasm, getRawKeystream} = require('./decrypt.cjs');
const input = process.stdin[Symbol.asyncIterator]();
let buffered = Buffer.alloc(0);
async function read(size) {
  const chunks=[];let remaining=size;
  while(remaining) {
    if(!buffered.length) {const next=await input.next();if(next.done)throw Error('incomplete frame');buffered=next.value;}
    const count=Math.min(buffered.length,remaining);chunks.push(buffered.subarray(0,count));buffered=buffered.subarray(count);remaining-=count;
  }
  return Buffer.concat(chunks,size);
}
async function write(body) {if(!process.stdout.write(body))await once(process.stdout,'drain');}
const mp4 = data => data.length >= 12 && data.subarray(4,8).toString() === 'ftyp';
async function main() {
  const size=(await read(4)).readUInt32BE();if(size<1||size>4096)throw Error('frame');
  const header=JSON.parse(await read(size));if(!Number.isInteger(header.size)||header.size<12||header.size>512*1024*1024)throw Error('frame');
  const window=Math.min(header.size,128*1024);const first=await read(window);
  if(!mp4(first)) {
    if(!/^\d{1,20}$/.test(header.seed))throw Error('key');
    const runtime=await initWasm(process.argv[2]);const stream=Buffer.from(await getRawKeystream(runtime,header.seed,window)).reverse();
    for(let n=0;n<first.length;n++)first[n]^=stream[n];if(!mp4(first))throw Error('video');
  }
  await write(first);let remaining=header.size-window;
  while(remaining) {const block=await read(Math.min(1024*1024,remaining));await write(block);remaining-=block.length;}
  if(buffered.length||!(await input.next()).done)throw Error('extra data');
}
main().catch(error=>{
  const code=['frame','key','video','incomplete frame','extra data'].includes(error.message)?error.message:'decoder_runtime';
  process.stderr.write(JSON.stringify({error_code:code})+'\n');process.exitCode=1;
});
