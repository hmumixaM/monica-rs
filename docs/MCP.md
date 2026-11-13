# AI access and local account recovery

In Settings → AI 访问 / MCP, enable MCP and create an access key for each client.
Keys can also be supplied explicitly (32–512 printable ASCII bytes), replaced,
or revoked. Only a digest is stored. The plaintext appears only when created
or replaced; copy the client configuration before closing it. Existing browser,
collector and supervisor tokens do not grant MCP access.

Connect with Streamable HTTP to the displayed `/mcp` address and an
`Authorization: Bearer <access-key>` header. MCP checks the enabled setting and
current key on every request. Existing sessions cannot keep using a revoked key.
The server supports standard initialization and stateless tool requests through
pinned official Rust SDK rmcp 3.10.0. A client configuration example is:

```json
{"mcpServers":{"monica":{"url":"http://127.0.0.1:17494/mcp","headers":{"Authorization":"Bearer REPLACE_WITH_YOUR_KEY"}}}}
```

The native application's port may differ when the preferred port is occupied.
Use its displayed address. For remote AI clients, configure an existing HTTPS
reverse proxy to forward `/mcp` to the local Rust backend. Preserve HTTP methods,
Authorization, Content-Type, Accept and MCP protocol headers. In MCP settings,
save the public HTTPS address ending exactly in `/mcp`; its authority and origin
are added to the transport's accepted hosts/origins. Configure forwarded Host to
match that authority. This does not create a tunnel, open a firewall, publish an
endpoint, or add OAuth discovery for clients that require OAuth instead of keys.
A reverse proxy must not expose `/internal/*`.

Tools read people, relationships, Memory, messages, versioned evidence, timelines,
coverage and operational status. Write tools create people, edit manual fields
(including notes), manage tags, and link/unlink source accounts. Linking supports
`expected_profile_id` to preserve the normal concurrent ownership check. Tools
use the same handlers, audit records, privacy rules and invalidation as the Web
UI. They cannot invoke arbitrary URLs, source controls, authentication or key
management. Query parameters are described in the tool schemas; UUIDs and actual
business bodies are validated by the existing APIs. Results include API references;
evidence data includes its versioned citation.

To recover a forgotten password, open the **native Mac settings window** and
choose 忘记密码 / 重置登录密码. The local supervisor reads the existing email and
submits the new password through its private control channel. The user UUID,
profiles, media, source credentials and work queues are retained; browser sessions
are revoked. Resetting TOTP/recovery codes is a separate, default-off checkbox.
An empty installation instead offers first-use account creation. Normal Web
password changes require the current password and also end existing sessions.

# Operational progress

Sources shows a concise summary and links to Imports. Imports shows source/account
state, typed pipeline backlogs, seven-source reconciliation, daily receipt counts,
paginated runs and a diagnostics dialog. Collection, durable receipt, query-ready
facts, media, Memory inputs and external model jobs are independent stages.

`/api/v1/imports/status`, `/runs`, `/runs/{id}`, `/runs/{id}/diagnostics` and `/daily`
require browser authentication (or are reached through the explicit MCP tools).
Daily counts are attributed to the receipt's local calendar date, using an IANA
timezone (browser timezone by default). UTC quarter-hour buckets preserve modern
IANA offset/day boundaries, including DST and quarter-hour zones. Daily summaries
survive detailed event pruning; operational events older than 90 days are removed
in bounded idle-maintenance batches. Original messages, evidence and import receipts
are not deleted. Missing historical telemetry is reported as no record.

Counters are written once per page/receipt, with signed connection pages tracked
only by their receipt. Connection receipts are not query-ready until their outbox
is drained. Compact account totals avoid history scans for status reads. Source
collector state is read only from the configured private collector root; no old
browser profile or account is discovered. Diagnostics include fixed operational
fields and phase transitions, without raw logs, cookies, credentials or message
payloads. Native settings still provides the local private-log entry point.

# Reproducible account-free checks

- `python3 scripts/test-cargo.py --workspace`
- `python3 scripts/frontend-pages-smoke.py` (build server/Web first)
- `npm run test --prefix apps/native-desktop`
- `npm install --prefix .local-validation/mcp-client --no-save --ignore-scripts @modelcontextprotocol/sdk@1.32.2`
- `python3 scripts/test-mcp-http.py` (release server first; actual official SDK,
  actual HTTP, isolated PostgreSQL and a forwarded-public-Host proxy fixture)

These checks never connect to NAS, log into Instagram, or invoke paid models.
