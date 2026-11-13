# Child-spool test readiness — 2026-11-13

Reviewed application source4c473f6f0c102a8bf8b7f8cdd1a62279435d3378; actual base301241bf41e8ba8bb18d981f4e9138fe220e9c18. Current native AMD64 CI passed, while a fresh local full run failed adapter_connections::independent_message_spool_parent_pause_keeps_page_replayable at the child-lease readiness assertion.

The test spawns a real consumer, then allows exactly100 database polls/yields to observe its committed running lease. That count supplies no elapsed startup budget for its filesystem and task scheduling. It can exhaust before the consumer reaches admission. This readiness check does not measure the substantive parent-pause/replay/ACK contract.

Use a bounded four-second wait for the same database-visible lease. If the consumer exits before admission, report its actual result immediately. Keep the import-table lock, receipt completion, paused-page retention, lack of ACK while paused and later resume/ACK assertions unchanged. This changes neither production code nor a performance/concurrency deadline. It replaces an iteration-count startup assumption with observable readiness.

Code-review and excessive-tests assessment: preserve the existing single integration scenario; add no duplicate test or sleep-based speed assertion. The prior failure log is retained as workspace-4c473f6-utc.log. Focused and full verification remain required before readiness is claimed.
