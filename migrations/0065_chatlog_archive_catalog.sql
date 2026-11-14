-- Every frozen SQLite table has an explicit conversion or archive disposition.
CREATE TABLE crm.legacy_chatlog_tables (
 snapshot_id uuid NOT NULL REFERENCES crm.legacy_chatlog_snapshots(id),
 file_name text NOT NULL,table_name text NOT NULL,mode text NOT NULL CHECK(mode IN ('business','history','derived')),
 expected bigint NOT NULL,processed bigint NOT NULL DEFAULT 0,after_row bigint,
 status text NOT NULL DEFAULT 'pending',reason text NOT NULL,
 PRIMARY KEY(snapshot_id,file_name,table_name)
);
CREATE TABLE crm.legacy_chatlog_history (
 snapshot_id uuid NOT NULL,file_name text NOT NULL,table_name text NOT NULL,row_key bigint NOT NULL,
 payload jsonb NOT NULL,
 PRIMARY KEY(snapshot_id,file_name,table_name,row_key),
 FOREIGN KEY(snapshot_id,file_name,table_name) REFERENCES crm.legacy_chatlog_tables(snapshot_id,file_name,table_name)
);
