# Preserve explicit imported message owners

Final source reconciliation found that a person-only CRM message could retain
its person during Chatlog reconciliation and then lose that person in the
finalizer. The source account observation and explicit person have separate
meaning. The CRM importer already removes account-derived duplicate ownership;
the finalizer must not clear an explicit person after an account is identified.

The repair removes that blanket cleanup. New migration 0067 recovers only rows
whose immutable CRM evidence supplied a person without an account, whose current
account still matches the Chatlog projection, and whose projection has no person
revision. Current nonempty people and saved person fields, including explicit
nulls, prevent automatic recovery. Existing migration contents remain unchanged.
The mutable projection records the recovered baseline and current value. Original
CRM evidence and immutable source revision bodies are unchanged. Deterministic
Memory work includes both previous owners and the recovered person.

Historical evidence can recover the original person-only attribution when an
older saved revision omitted that field. An explicitly saved field still wins.
The visible participant name follows the selected person.

Validation extends the existing PostgreSQL contract through person-only import,
source-account reconciliation, actual finalization and CRM verification. It also
executes the repair migration on the old damaged shape, checks explicit-null and
current-person protections, previous/new Memory owners and replay. Existing HTTP
coverage checks selected historical people and current manual names.

The real frozen Mac copy has 81,094 rows matching the guarded recovery shape.
Their original people remain in immutable evidence. This is a pre-application
finding, not a claim that recovery, NAS restore or complete file verification has
finished. Deployment and final source reconciliation require the reviewed repair.
