# Imported group speech without a live Chatlog query dependency

The person group-speech dialog now reads the Rust database only. It does not
require MONICA_CHATLOG_ALPHA_URL, a live Chatlog HTTP service, a WeChat account
link, or network reads. Acquisition/decryption and folder reception are separate
and remain unchanged. New collected content appears after its import finishes.

Room discovery follows actual sender attribution across supported providers.
Native explicit senders take precedence over legacy source-account links. Rooms
without a linked group profile remain readable and pageable by conversation ID.
The dialog shares indexed message reads, source revision citations and bounded
media loading with person messages and Agent/MCP tools.

`GET /api/v1/profiles/{id}/group-speech` returns local room pages with
conversation_id, optional group_id, provider, messages and per-room next cursors.
Pass conversation_id plus before/before_id to continue older messages. These
stable cursors replace the former live Chatlog date-window reads. group_offset
continues room pagination; group_id, q, since and until filter imported content.
Calendar-day filters preserve the installation's Chicago convention (until day
inclusive); RFC3339 interval ends are exclusive. Responses identify the scope as
imported_records rather than claiming that all historical source data is present.

Each request has a five-second overall budget and bounded per-room message
pages. No source row is explicitly locked. The UI preserves identical text/time
messages by stable message ID, aborts superseded/closed reads and formats times
in the viewer's timezone. Empty imported results are ordinary empty states.

Validation uses isolated SQLx data, synthetic browser responses and the existing
real Rust pages runner. It covers equal-time cursors, day/search filters, explicit
sender precedence, no WeChat link, unlinked native rooms and an empty person
without live Chatlog configuration. No collector or real model is run. Existing
NAS imports and media copying are unaffected; this source is deployed only after
the pinned import controllers can safely accept a new service version.
