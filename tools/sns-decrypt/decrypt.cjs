// Keystream loader from pinned MIT-licensed Chatlog 53ab86b7fe3c.
// Private seeds are stdin-only. No network or logging.
const fs = require('fs');
const path = require('path');
const vm = require('vm');

async function initWasm(basePath) {
  const wasmPath = path.join(basePath, 'wasm_video_decode.wasm');
  const jsPath = path.join(basePath, 'wasm_video_decode.js');
  const wasmBinary = fs.readFileSync(wasmPath);
  let capturedKeystream = null;

  return await new Promise((resolve, reject) => {
    try {
      const mockGlobal = {
        console,
        Buffer,
        Uint8Array,
        Int8Array,
        Uint16Array,
        Int16Array,
        Uint32Array,
        Int32Array,
        Float32Array,
        Float64Array,
        BigInt64Array,
        BigUint64Array,
        Array,
        Object,
        Function,
        String,
        Number,
        Boolean,
        Error,
        Promise,
        require,
        process,
        setTimeout,
        clearTimeout,
        setInterval,
        clearInterval,
      };

      mockGlobal.Module = {
        onRuntimeInitialized: () => resolve({
          Module: mockGlobal.Module,
          getCapturedKeystream: () => capturedKeystream,
          resetCapturedKeystream: () => { capturedKeystream = null; }
        }),
        wasmBinary,
        print: () => {},
        printErr: () => {},
      };

      mockGlobal.self = mockGlobal;
      mockGlobal.self.location = { href: jsPath };
      mockGlobal.WorkerGlobalScope = function () {};
      mockGlobal.VTS_WASM_URL = `file://${wasmPath}`;
      mockGlobal.wasm_isaac_generate = (ptr, size) => {
        const buf = new Uint8Array(mockGlobal.Module.HEAPU8.buffer, ptr, size);
        capturedKeystream = new Uint8Array(buf);
      };

      const jsContent = fs.readFileSync(jsPath, 'utf8');
      const script = new vm.Script(jsContent, { filename: jsPath });
      const context = vm.createContext(mockGlobal);
      script.runInContext(context);
    } catch (error) {
      reject(error);
    }
  });
}

async function getRawKeystream(runtime, key, size) {
  const mod = runtime.Module;
  if (!mod.WxIsaac64 && mod.asm && mod.asm.WxIsaac64) {
    mod.WxIsaac64 = mod.asm.WxIsaac64;
  }
  if (!mod.WxIsaac64) {
    throw new Error('WxIsaac64 not found in WASM module');
  }
  runtime.resetCapturedKeystream();
  const isaac = new mod.WxIsaac64(key);
  isaac.generate(size);
  if (isaac.delete) isaac.delete();
  const ks = runtime.getCapturedKeystream();
  if (!ks) {
    throw new Error('Failed to capture keystream');
  }
  return Buffer.from(ks);
}

async function main() {
 const body=fs.readFileSync(0);
 if(body.length<4)throw Error('frame');
 const length=body.readUInt32BE(0);
 if(length<1||length>4096||body.length<4+length)throw Error('frame');
 const request=JSON.parse(body.subarray(4,4+length));
 if(!Number.isInteger(request.size)||request.size<1||request.size>32*1024*1024)throw Error('frame');
 const input=body.subarray(4+length);
 if(input.length!==request.size)throw Error('frame');
 // Chatlog's SNS cache already contains decoded bytes. Older CDN images can
 // also be plain JPEG/PNG. They must not pass through the keystream twice.
 if(input.subarray(0,3).equals(Buffer.from([255,216,255]))||input.subarray(0,8).equals(Buffer.from([137,80,78,71,13,10,26,10]))) {process.stdout.write(input);return;}
 if(!/^\d{1,20}$/.test(request.seed))throw Error('frame');
 const runtime=await initWasm(process.argv[2]);
 const raw=await getRawKeystream(runtime,request.seed,Math.ceil(input.length/8)*8);
 for(const reversed of [true,false]) {
  const stream=Buffer.from(raw);if(reversed)stream.reverse();
  const output=Buffer.alloc(input.length);for(let n=0;n<input.length;n++)output[n]=input[n]^stream[n];
  if(output.subarray(0,3).equals(Buffer.from([255,216,255]))||output.subarray(0,8).equals(Buffer.from([137,80,78,71,13,10,26,10]))) {process.stdout.write(output);return;}
 }
 throw Error('image');
}
module.exports = { initWasm, getRawKeystream };
if (require.main === module) main().catch(()=>{process.stderr.write('sns_decode_failed\n');process.exit(1)});
