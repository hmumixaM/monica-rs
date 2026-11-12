-- Operational state is separate from the active task rows and task history.
CREATE TABLE crm.scheduler_control (
    singleton BOOLEAN PRIMARY KEY DEFAULT true CHECK (singleton),
    paused BOOLEAN NOT NULL DEFAULT false,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
INSERT INTO crm.scheduler_control(singleton) VALUES(true);

CREATE TABLE crm.scheduler_failures (
    queue TEXT NOT NULL CHECK (queue IN ('object', 'summary')),
    work_key TEXT NOT NULL,
    failures INTEGER NOT NULL CHECK (failures > 0),
    retry_at TIMESTAMPTZ NOT NULL,
    quarantined BOOLEAN NOT NULL DEFAULT false,
    error_code TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY(queue, work_key)
);
CREATE INDEX scheduler_retry_due ON crm.scheduler_failures(retry_at)
    WHERE NOT quarantined;

CREATE TABLE crm.scheduler_runs (
    instance_id UUID PRIMARY KEY,
    started_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    heartbeat_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    stopped_at TIMESTAMPTZ,
    partitions INTEGER NOT NULL CHECK(partitions > 0),
    active_jobs INTEGER NOT NULL DEFAULT 0,
    completed_jobs BIGINT NOT NULL DEFAULT 0,
    failed_attempts BIGINT NOT NULL DEFAULT 0,
    import_recovery_attempts BIGINT NOT NULL DEFAULT 0,
    state TEXT NOT NULL CHECK(state IN ('running','paused','draining','stopping','stopped'))
);
