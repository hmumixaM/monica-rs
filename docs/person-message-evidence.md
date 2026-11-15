# Person message scope and immutable citations

`GET /api/v1/profiles/{id}/messages?include_group_speech=true` combines direct
messages with that person's own group speech. The default retains the previous
profile-conversation scope. Group/profile pages and explicit conversation reads
still show the requested conversation. Search, month, conversation type, anchor,
time/ID pagination and tombstones apply before each candidate stream's limit.
An explicit native sender takes precedence over a legacy account link.

Candidate streams carry only IDs/times and are bounded before full message,
name and evidence joins. Media still joins after the global page is bounded.
New partial author/time indexes support native and legacy attribution. No source
facts are explicitly row-locked. The new indexes use a fresh migration.

Native message bodies and their evidence revision are selected in one database
statement, preventing a body from one revision being paired with another revision
read afterward. The API returns evidence_id, evidence_revision and a versioned
citation. Browser Agent tools preserve both native and migrated legacy citations;
they only fall back to conversation links when no original citation exists.

Focused checks use an isolated SQLx database and browser model stubs: newer
unrelated room messages cannot hide older wanted speech, same-time pagination
keeps both IDs, filtering and sender precedence hold, tombstones disappear, native
edits retain the first raw version, and actual Agent tool replies retain versioned
links through the browser loop and saved sessions. No real AI call or live
Chatlog connector is required. Real collectors and model execution remain off
until the ongoing full import completes.
