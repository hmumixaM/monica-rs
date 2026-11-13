# Synology CPU controls acceptance finding — 2026-11-13

Reviewed application/source head: `cb8bdeb7d5aff5390891f6a34ad1bcbea9e1d56c`; branch baseline `301241bf41e8ba8bb18d981f4e9138fe220e9c18`.

## C3 [medium] Avoid unsupported NAS CFS quotas

The four optional WeChat/Instagram/LinkedIn collector services in `infra/compose.yaml` specify `cpus: 1`. Docker translates that to NanoCPUs/CFS quota. Actual isolated NAS container creation failed with `NanoCPUs can not be set, as your kernel does not support CPU CFS scheduler or the cgroup is not mounted`. Read-only Docker capability evidence confirms `CpuCfsPeriod=false`, `CpuCfsQuota=false`, `CPUSet=true`, `CPUShares=true`, `MemoryLimit=true` on this Synology NAS. Thus these optional sources cannot start using the supplied Compose configuration, even though Mac-dependent Apple readers are correctly disabled.

Repair: use a configurable cpuset, defaulting to CPU 0, for those optional collectors. This bounds each collector to one available CPU on both target hosts without requiring CFS quota support. Users can explicitly set `MONICA_COLLECTOR_CPUSET` for a different valid CPU set. Preserve the existing memory limit, disabled-by-default profiles and source controls. The isolated NAS test runner likewise pins its own test CPUs instead of requesting unsupported quotas.

Code-review traced Compose-to-Docker resource settings and the actual daemon capabilities. Validation must render all optional profiles and verify actual NAS cpuset container creation. No production container, existing Compose configuration, account, source session or data was changed. No source-text unit test is needed for a deployment resource field; real daemon creation is the relevant contract.
