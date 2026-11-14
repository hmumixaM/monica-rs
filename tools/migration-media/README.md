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

The pinned Chatlog module matches desktop commit
`53ab86b7fe3c`; exact module versions and checksums are in `go.mod` and `go.sum`.
Wrapper license notices are under `licenses/`. The SILK C decoder includes its
Skype BSD notice. The CGO MP3 encoder embeds the upstream LAME C implementation,
which is licensed under the GNU Library General Public License v2 or later,
separately from the Go wrapper's MIT license. The unmodified corresponding source
is the pinned `github.com/sjzar/go-lame v0.0.9` module's `clame/` tree. Preserve that
source, its copyright headers and the upstream library license when distributing
a CGO executable. This helper is not included in the Rust runtime container.
