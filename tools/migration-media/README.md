# Offline migration codecs

This one-shot helper works on independent frozen copies. It never discovers a
live Chatlog process, extracts keys, calls models or starts collection. Supply
explicit disjoint roots, and create `.monica-rust-migration-media` in the new
output root with the exact contents `monica-rust-isolated-media-v1\n`. Input is a
stream of JSON objects containing a relative `path` and `kind` (`image`, `voice`,
`video` or `file`). Output contains only content hashes and relative CAS paths.

Build and test with `go test ./...` and `go build .`. Voice conversion requires
CGO; CGO-disabled builds can copy files and decode images but must report an
unsupported voice codec. Voice source blobs remain in the sealed SQLite copy.
Image credentials, when needed, are process environment values only. Never put
them in a command argument, a manifest or a log. The helper removes those
variables before spawning any image-conversion subprocess.

Returned errors exit nonzero and write one JSON record to stderr with `error_code`
and `codec_commit`. The fixed codes distinguish image conversion, WXGF
conversion, decoded image validation and voice conversion. Other failures use
`media_decode_failed`. They identify the failed stage; they do not assert that
a source file is corrupt or that a key is wrong. The JSON report never includes
raw errors, keys, paths or decrypted content. Successful stdout remains the same asset
record. Retry callers must retain the raw-file receipt and failure record and
must not mark a failed conversion ready.

Uncaught panics or native dependencies can produce other diagnostics. Callers
must accept only validated JSON fields and must not save captured stdout or
stderr verbatim as a diagnostic record.

`--batch --credentials-stdin` adds an explicit recovery protocol. The first
newline-terminated JSON frame contains only `image_key` and `image_xor` strings.
It is consumed in memory, and credential environment variables are removed
before conversion. Each subsequent frame contains a unique 64-character hex
`id`, relative `path`, and `kind`. A batch accepts at most 128 requests, each
frame at most 8192 bytes. Responses contain `id`, `codec_commit`, and either an
`asset` with the ordinary successful fields or a fixed `error_code`. Conversion
failures remain individual results; malformed input fails the batch process.
The existing single-request protocol remains the default.

The indexed-media recovery client is `scripts/retry-wechat-media.py`; see
`docs/indexed-media-recovery.md`. It uses a pinned local runtime with FFmpeg,
read-only copied raw files, a marked CAS destination, and no network. Do not
replace an active copy worker with this client.

Stamp release builds from the reviewed clean source archive with
`go build -trimpath -ldflags "-X main.buildCommit=FULL_REVIEWED_COMMIT" .`.
An unstamped development build reports `development`; neither that label nor
Go's inherited VCS metadata proves correspondence to a deployed binary. Keep
the source archive, build inputs and final binary SHA-256 together. Do not
replace the codec or worker in an active import; diagnose and retry a bounded
set after the current worker exits, reusing its verified raw copies.

The pinned Chatlog module matches desktop commit
`53ab86b7fe3c`; exact module versions and checksums are in `go.mod` and `go.sum`.
Wrapper license notices are under `licenses/`. The SILK C decoder includes its
Skype BSD notice. The CGO MP3 encoder embeds the upstream LAME C implementation,
which is licensed under the GNU Library General Public License v2 or later,
separately from the Go wrapper's MIT license. The unmodified corresponding source
is the pinned `github.com/sjzar/go-lame v0.0.9` module's `clame/` tree. Preserve that
source, its copyright headers and the upstream library license when distributing
a CGO executable. This helper is not included in the Rust runtime container.
