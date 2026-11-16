# Weekly message statistics

Home and Insights read `crm.weekly_message_statistics`, a durable snapshot seeded
by migration 87. They never rescan message history or live message-count tables
to produce message totals. Restarting the web server does not discard this data.

The maintenance worker checks whether a refresh is due every five minutes. A new
snapshot is built at most once per calendar week, starting Monday in
`America/New_York`; a check within the same week returns without rebuilding it.
This job does not run image audits or make model/network calls. Concurrent
refreshes coalesce through a transaction advisory lock. A failed transaction
leaves the previous successful snapshot intact.

Counts retain the current non-deleted sent/received semantics. The 30-day window
is anchored at `statistics_updated_at`, returned by Home and Insights. Names,
profile visibility, archived state, ownership and last-contact timestamps are
still read from current rows. Exact message pages continue to use current data.

Birthday and anniversary reminders use authenticated `/api/v1/home/dates` and
load independently of Insights and model suggestions. Date edits invalidate
their cache immediately; date calculations remain current rather than weekly.
