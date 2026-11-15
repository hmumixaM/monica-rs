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

Failures exit nonzero and write one JSON record to stderr with `error_code`
and `codec_commit`. The fixed codes distinguish image conversion, WXGF
conversion, decoded image validation and voice conversion. Other failures use
`media_decode_failed`. They identify the failed stage; they do not assert that
a source file is corrupt or that a key is wrong. Raw errors, keys, paths and
decrypted content are never included. Successful stdout remains the same asset
record. Retry callers must retain the raw-file receipt and failure record and
must not mark a failed conversion ready.

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
