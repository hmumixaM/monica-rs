# QuickJS WebAssembly

`quickjs.wasm` is `dist/emscripten-module.wasm` from
[`@jitl/quickjs-wasmfile-release-sync@0.32.0`](https://www.npmjs.com/package/@jitl/quickjs-wasmfile-release-sync/v/0.32.0).
Its SHA-256 is `105c3bed22d457e43e3d1c3c1c6959fda62a8fe06f0fc8a985303c3a2be72232`.
Use `npm pack @jitl/quickjs-wasmfile-release-sync@0.32.0` to obtain the original
archive. The release's Emscripten glue documents the minified FFI used by
`sandbox.rs`. Review the imports and run the sandbox tests when updating it.
The Rust server embeds the module, including in NAS and desktop builds.
