# Independent test assessment: sender settlement

Reviewed commit: `2db879447000276195cf89641ace9ebbc1dae839`. Narrow scope: existing attribution-related coverage in `scripts/test-legacy-import.py` and `crates/monica-server/src/legacy_tests.rs`, with production mapping/finalizer contracts traced.

No excessive-test finding. Existing synthetic import/resume, immutable evidence, selected historical author, source-account relinking, direct manual-person precedence, and known/unknown sender checks protect distinct meaningful contracts. No removal or broad suite rewrite is warranted.

There is a focused missing regression corresponding to confirmed [Bugbot B1](./2026-11-14-bugbot-report-0fe904d1.md): original CRM account NULL + explicit person, followed by Chatlog known account correction and actual finalization. Current setup starts with a non-null source account, so it cannot expose the destructive transition. The HTTP manual-person test sets the person after import and reads it directly without finalizing again, so it also misses this SQL effect.

Proportionate repair coverage: extend the existing synthetic importer test with person-only -> account-filled -> finalize and CRM verify, assert explicit person survives even if the account links to a different person, and keep immutable evidence unchanged. For any new backfill, test only the guards that prevent data loss: preserve current manual non-null value; preserve projection profile key including null; skip account/projection mismatch; restore the proven blank current value; rerun idempotently. These are distinct persisted-ownership boundaries, not implementation duplication. Do not add performance tests or another broad test framework for this fix.

Independent verification used an isolated PostgreSQL 16 instance and the real correction method plus exact finalizer batch SQL; it reproduced the missing contract failure in 1.1 seconds. This report does not claim the full integration suite passed.
