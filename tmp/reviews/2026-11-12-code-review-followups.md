# Code review follow-up findings

Review scope: Rust working tree at 5fdd09b plus local changes, following initial Bugbot bb-20261112-4e29e212. The user requested all verified issues repaired and macOS/NAS feature parity. This is the deeper code-review phase, not another Bugbot detection loop.

- C1 high — `crates/monica-core/src/ai.rs` digest validation saves only `facts.sources`; `crates/monica-server/src/ai.rs` digest returns no `people` or `post_dates`. `apps/web/components/digest/digest-view.tsx:42` dereferences `facts.top_contacts[0]`. Any generated digest crashes. Missing SQL week counts, new people/links, upcoming dates and dormant facts also remove existing Python functionality. Preserve snapshot facts independently of model prose and return the complete contract.
- C2 high — Model inputs in `ai.rs`, `ai_memory.rs`, and `migrations/0042_legacy_memory.sql` strip message direction and author attribution. Both direct-chat speakers become indistinguishable; group speech is absent from the regular profile/week input. The model cannot reliably distinguish the user's statements from the contact's. Preserve authoritative sender/direction, use only the person's group speech, and return usable source citations.
- C3 medium — `ai.rs` includes all current profile fields as evidence in `ai.person_week`, regardless of the requested week. The provider prompt does not restrict weekly claims to actual events in that week. Old information can become a weekly update. Limit weekly evidence to in-week events and use explicit weekly instructions.

Test audit: the empty-read differential compares wall-clock `as_of` and requires Rust's integrated coordinator list to equal Python's separate-worker empty list. These are test-oracle mismatches. Validate timestamp format and the coordinator separately while retaining exact business-body equality.

Implementation self-review: the new model settings save button must explicitly use submit because the shared Button defaults to type=button. A real browser acceptance test exposed this; fix and retain that acceptance test.

This is a fixed finding record. Repair and final validation are recorded separately.
