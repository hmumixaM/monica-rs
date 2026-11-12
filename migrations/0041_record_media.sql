CREATE TABLE crm.import_record_media_progress (
 batch_id uuid PRIMARY KEY REFERENCES crm.import_batches(id),
 record_offset bigint NOT NULL DEFAULT 0, media_offset bigint NOT NULL DEFAULT 0,
 lease_token uuid, lease_until timestamptz,
 ordinal integer NOT NULL DEFAULT 0, complete boolean NOT NULL DEFAULT false,
 updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX import_record_media_pending ON crm.import_record_media_progress(updated_at,batch_id) WHERE NOT complete;
-- Existing durable receipts are backfilled once; new receipts enqueue in their
-- acceptance transaction, so idle recovery never scans the full receipt history.
INSERT INTO crm.import_record_media_progress(batch_id)
SELECT batch_id FROM crm.import_batch_inputs WHERE payload @? '$.records[*].media[*]';
