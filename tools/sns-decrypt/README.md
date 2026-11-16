# SNS image runtime

`fetch_frame.py` reads one explicit SNS image from a read-only SQLite snapshot,
uses the existing Chatlog cache first, then downloads the source CDN reference.
It streams a private length-prefixed JSON header plus image bytes. The header's
image seed must never be logged or saved.

`decrypt.cjs` accepts that frame on stdin, passes already decoded images through,
or uses the pinned Chatlog WASM keystream. It emits only decoded bytes. `main.go`
fully decodes JPEG/PNG pixels and emits SHA-256, byte length, MIME and dimensions.

The worker receives a separate immutable `--sns-runtime` directory containing:

- `node-run`, `node` and its Linux loader/library closure;
- the statically built `validate` executable from this Go module;
- `wasm/wasm_video_decode.js` and `wasm/wasm_video_decode.wasm`.

WASM assets are from the locally pinned Chatlog source revision `53ab86b7fe3c`,
module version `v0.1.0-alpha.1.0.20260619111250-53ab86b7fe3c`:

| File | SHA-256 |
| --- | --- |
| `wasm_video_decode.js` | `78faf7621959e30ba05c0acf7182dd14bcbd7dfe45529476649f39b20e5dbea3` |
| `wasm_video_decode.wasm` | `dca796bacec37d8522c7983b3945e5d579bd74164e3b21f0ebc773be6dfc8b6e` |

The loader uses the pinned MIT-licensed Chatlog helper. No image seeds are passed
as arguments, environment variables, logs, or runtime receipt fields. Downloads
and pixel decoding finish before publication takes database locks. Source-page
replays reuse decoded staging; this is not a periodic media audit.
