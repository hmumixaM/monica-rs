#!/usr/bin/env python3
"""Offline, resumable migration from four read-only Monica Next copies.

Run with uv --with 'psycopg[binary]==3.2.12' --with cryptography==46.0.3.
Connection strings and installation encryption secrets come from environment
variables, never command arguments or the public migration manifest.
"""
import argparse
import base64
import hashlib
import hmac
import json
import os
import shutil
from pathlib import Path
import sys
import uuid

import psycopg
from psycopg import sql
from psycopg.types.json import Jsonb
from cryptography.fernet import Fernet

STORES = ("crm", "wechat", "instagram", "linkedin")
NS = uuid.UUID("534b6268-97a1-4d99-a086-f8c9e3955815")
DIRECT = {
    "media_assets": "media_assets", "profiles": "profiles", "app_users": "app_users",
    "audit_events": "audit_events", "tags": "tags", "source_accounts": "source_accounts",
    "source_account_links": "source_account_links",
    "source_account_link_revisions": "source_account_link_revisions",
    "identities": "identities", "identity_candidates": "identity_candidates",
    "profile_redirects": "profile_redirects", "group_memberships": "group_memberships",
    "source_group_memberships": "source_group_memberships", "profile_dates": "profile_dates",
    "tag_memberships": "tag_memberships", "tag_membership_removals": "tag_membership_removals",
    "media_links": "media_links",
    "activities": "activities", "activity_participants": "activity_participants",

    "faces": "faces", "face_scans": "face_scans", "image_ocr_readings": "image_ocr_readings",
    "profile_suggestion_decisions": "profile_suggestion_decisions",
    "community_names": "community_names", "linkedin_collection_targets": "linkedin_collection_targets",
    "auth_recovery_codes": "auth_recovery_codes",
    "source_chat_months":"source_chat_months",
}
TRANSFORMED = {
    "content_objects", "profile_field_revisions", "profile_field_current", "source_account_fields",
    "companies", "schools", "profile_address_revisions", "profile_employment_revisions",
    "profile_education_revisions", "profile_structured_current", "source_account_facts",
    "conversations", "message_events", "social_posts", "social_stories", "social_interactions",
    "relationship_edges", "platform_link_decisions", "source_contact_events", "ai_outputs",
    "person_weekly_updates", "weekly_digests", "social_post_media_readings", "social_story_media_readings",
    "reconnect_states", "source_account_media", "source_account_tags",
    "activity_revisions", "relationship_edge_revisions", "tag_membership_revisions",
    "source_relationship_edges", "social_post_revisions", "social_story_revisions",
}
HISTORY = {
    "source_observations", "social_comments",
    "source_crosswalk", "source_crosswalk_revisions", "group_membership_revisions",
    "repair_snapshots", "media_source_revisions", "media_manifest",
    "import_batches", "source_cursors", "jobs", "scheduled_runs", "source_runtime",
    "worker_heartbeats", "model_call_slots", "image_ocr_daily_usage", "webauthn_credentials",
    "webauthn_challenges", "sessions", "auth_login_attempts", "import_service_tokens",
    "import_request_nonces", "media_uploads", "remote_media_sources", "source_checkpoints",
}
DERIVED = {
    "schema_migrations", "search_documents", "message_search", "message_source_cursors",
    "source_reconciliation", "source_profile_projections", "platform_migration_state",
    "social_post_locations", "social_story_locations", "profile_birthdays",
    "profile_metrics_current", "global_metric_snapshots", "profile_metric_values",
    "archive_media_files", "conversation_message_months", "conversation_stats_dirty",
    "conversation_sender_stats", "data_version_events", "data_version_totals", "image_ocr_settings",
    "relationship_types",
}
PROVIDER_TABLES = {"accounts", "account_identifiers", "payloads", "objects", "revisions",
                   "observations", "media", "media_links", "migration_cursors"}


def canonical(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def digest(value):
    return hashlib.sha256(canonical(value).encode()).hexdigest()


def connect_source(store):
    c = psycopg.connect(os.environ[f"MONICA_LEGACY_{store.upper()}_URL"], autocommit=True,
                        options="-c default_transaction_read_only=on -c statement_timeout=120000 -c lock_timeout=1000")
    if c.execute("SHOW default_transaction_read_only").fetchone()[0] != "on":
        raise RuntimeError("source must be read only")
    # Conversion operates only on restored copies on a private local socket.
    if c.info.host and not c.info.host.startswith("/"):
        raise RuntimeError("source must be an offline local Unix-socket copy")
    return c


def tables(c):
    return [r[0] for r in c.execute("SELECT c.relname FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='public' AND c.relkind IN ('r','p') AND NOT c.relispartition ORDER BY c.relname")]


def columns(c, schema, table):
    return [r[0] for r in c.execute("SELECT column_name FROM information_schema.columns WHERE table_schema=%s AND table_name=%s ORDER BY ordinal_position", (schema, table))]


def primary_keys(c, table, schema="public"):
    return [r[0] for r in c.execute("SELECT a.attname FROM pg_index i CROSS JOIN LATERAL unnest(i.indkey) WITH ORDINALITY k(attnum,pos) JOIN pg_attribute a ON a.attrelid=i.indrelid AND a.attnum=k.attnum WHERE i.indrelid=%s::regclass AND i.indisprimary ORDER BY k.pos", (schema+"."+table,))]


def mode(store, table):
    if store != "crm":
        return "history" if table == "migration_cursors" else "business" if table in PROVIDER_TABLES else "unknown"
    if table in DIRECT or table in TRANSFORMED:
        return "business"
    if table in HISTORY:
        return "history"
    if table in DERIVED:
        return "rebuild"
    return "unknown"


def inspect_sources(sources):
    manifest = {"format": 1, "config":{"wechat_self_id":os.environ.get("MONICA_LEGACY_WECHAT_SELF_ID") or None},"stores": {}}
    for store, c in sources.items():
        inventory = []
        for table in tables(c):
            count=0;hasher=hashlib.sha256()
            keys=primary_keys(c,table)
            order=sql.SQL(',').join(map(sql.Identifier,keys)) if keys else sql.SQL('ctid')
            with c.transaction():
                with c.cursor(name="legacy_fingerprint") as cursor:
                    cursor.execute(sql.SQL("SELECT to_jsonb(t)::text FROM public.{} t ORDER BY {}").format(sql.Identifier(table),order))
                    for row in cursor:
                        data=row[0].encode();hasher.update(len(data).to_bytes(8,"big"));hasher.update(data);count+=1
            inventory.append({"table": table, "mode": mode(store, table), "rows": count,
                              "sha256":hasher.hexdigest(),"columns": columns(c, "public", table), "keys": keys})
        manifest["stores"][store] = {"database": c.info.dbname, "tables": inventory}
    unknown = [f"{s}.{t['table']}" for s, v in manifest["stores"].items()
               for t in v["tables"] if t["mode"] == "unknown"]
    if unknown:
        raise RuntimeError("unclassified source tables: " + ", ".join(unknown))
    return manifest


class Importer:
    def __init__(self, sources, target, manifest):
        self.sources, self.target, self.manifest = sources, target, manifest
        self.hash = digest(manifest)
        self.id = uuid.uuid5(NS, "legacy-migration:"+self.hash)
        self.target_columns = {}
        self.required_json = {}
        self.organization_map = {}
        self.verifying=False
        self.source_memberships=None
        self.metadata_spec={}
        self.source_metadata_spec={}
        self.metadata_only=False
        self.space_path=None
        for table, kind in [("companies", "company"), ("schools", "school")]:
            for key, name in sources["crm"].execute(sql.SQL("SELECT id,normalized_name FROM public.{}").format(sql.Identifier(table))):
                self.organization_map[str(key)] = str(uuid.uuid5(uuid.NAMESPACE_URL, f"monica:{kind}:{name}"))

    def guard_target(self, create=True):
        if not self.target.info.dbname.startswith("monica_rust"):
            raise RuntimeError("target database must use the dedicated monica_rust prefix")
        target_identity = self.target.execute("SELECT system_identifier FROM pg_control_system()").fetchone()[0]
        for source in self.sources.values():
            identity = source.execute("SELECT system_identifier FROM pg_control_system()").fetchone()[0]
            if (identity, source.info.dbname) == (target_identity, self.target.info.dbname):
                raise RuntimeError("source and target database are identical")
        active = self.target.execute("SELECT manifest_hash,status FROM crm.legacy_migrations").fetchall()
        if any(h != self.hash for h, _ in active):
            raise RuntimeError("target belongs to a different snapshot")
        if not active:
            if not create:
                raise RuntimeError("target has no migration record")
            populated = self.target.execute("SELECT EXISTS(SELECT 1 FROM crm.profiles) OR EXISTS(SELECT 1 FROM crm.source_accounts) OR EXISTS(SELECT 1 FROM crm.content_objects)").fetchone()[0]
            if populated:
                raise RuntimeError("initial target must be empty")
        if not create:
            return
        self.target.execute("INSERT INTO crm.legacy_migrations(id,manifest_hash,source_manifest) VALUES(%s,%s,%s) ON CONFLICT DO NOTHING", (self.id, self.hash, Jsonb(self.manifest)))
        self.target.execute("UPDATE crm.ai_settings SET enabled=true,execution_enabled=false,qa_enabled=false,include_messages=true,include_private_notes=true WHERE id")
        self.target.execute("UPDATE crm.image_ocr_settings SET enabled=false WHERE id")
        total=sum(t["rows"] for v in self.manifest["stores"].values() for t in v["tables"] if t["mode"]!="rebuild")
        self.target.execute("INSERT INTO crm.import_activity(id,provider,stream,kind,state,phase,received_at,records) VALUES(%s,'legacy','migration','migration','running','convert',now(),%s) ON CONFLICT DO NOTHING",("migration:"+str(self.id),total))
        for store, value in self.manifest["stores"].items():
            for t in value["tables"]:
                self.target.execute("INSERT INTO crm.legacy_migration_tables(migration_id,store,table_name,mode,expected) VALUES(%s,%s,%s,%s,%s) ON CONFLICT DO NOTHING", (self.id, store, t["table"], t["mode"], t["rows"]))

    def insert(self, table, rows, schema="crm"):
        if not rows:
            return
        if self.verifying and schema=="crm" and table in {"message_search","timeline_events","social_interactions"}:
            kind,key={"message_search":("message","message_id"),"timeline_events":("post","id"),"social_interactions":("interaction","id")}[table]
            updates={str(i):(old,new) for i,old,new in self.target.execute("SELECT entity_id,original_row,current_row FROM crm.legacy_chatlog_projection WHERE entity_type=%s AND entity_id=ANY(%s::uuid[])",(kind,[r[key] for r in rows]))}
            for r in rows:
                if str(r[key]) in updates:
                    old,new=updates[str(r[key])]
                    if any(k in r and r[k]!=v for k,v in old.items()):
                        raise RuntimeError("Chatlog correction lost its original CRM baseline")
                    r.update(new)
        key = (schema, table)
        if key not in self.target_columns:
            self.target_columns[key] = columns(self.target, schema, table)
            self.required_json[key] = {r[0] for r in self.target.execute("SELECT column_name FROM information_schema.columns WHERE table_schema=%s AND table_name=%s AND udt_name='jsonb' AND is_nullable='NO'",(schema,table))}
        fields = [k for k in self.target_columns[key] if k in rows[0]]
        if not fields:
            raise RuntimeError("no mapped target columns for "+schema+"."+table)
        if any(set(r) != set(rows[0]) for r in rows):
            raise RuntimeError("inconsistent migration record shape")
        names = sql.SQL(",").join(map(sql.Identifier, fields))
        expressions=sql.SQL(",").join(sql.SQL("coalesce(j.{},'null'::jsonb) AS {}").format(sql.Identifier(f),sql.Identifier(f)) if f in self.required_json[key] else sql.SQL("j.{}").format(sql.Identifier(f)) for f in fields)
        if self.verifying:
            if table=="memory_legacy_pending_records":
                return  # Deterministic inputs are drained before acceptance.
            compared=[f for f in fields if not (table=="conversations" and f in {"message_count","last_message_at","last_message_preview"})]
            keys=primary_keys(self.target,table,schema)
            if not keys or not set(keys).issubset(fields):
                raise RuntimeError("verification requires mapped target primary key for "+schema+"."+table)
            predicate=sql.SQL(" AND ").join([sql.SQL("a.{}=e.{}").format(sql.Identifier(f),sql.Identifier(f)) for f in keys]+[sql.SQL("a.{} IS NOT DISTINCT FROM e.{}").format(sql.Identifier(f),sql.Identifier(f)) for f in compared])
            query=sql.SQL("WITH expected AS(SELECT {} FROM jsonb_populate_recordset(NULL::{}.{},%s) j) SELECT count(*) FROM expected e WHERE NOT EXISTS(SELECT 1 FROM {}.{} a WHERE {})").format(expressions,sql.Identifier(schema),sql.Identifier(table),sql.Identifier(schema),sql.Identifier(table),predicate)
            if self.target.execute(query,(Jsonb(rows),)).fetchone()[0]:
                raise RuntimeError("target content mismatch in "+schema+"."+table)
            return
        query = sql.SQL("INSERT INTO {}.{} ({}) OVERRIDING SYSTEM VALUE SELECT {} FROM jsonb_populate_recordset(NULL::{}.{},%s) j ON CONFLICT DO NOTHING").format(sql.Identifier(schema), sql.Identifier(table), names, expressions, sql.Identifier(schema), sql.Identifier(table))
        self.target.execute(query, (Jsonb(rows),))

    def content(self, records, key="content_object_id"):
        ids = list({r[key] for r in records if r.get(key)})
        if not ids:
            return {}
        return {str(k): v for k, v in self.sources["crm"].execute("SELECT id,payload_json FROM public.content_objects WHERE id=ANY(%s::uuid[])", (ids,))}

    @staticmethod
    def is_uuid(value):
        try:
            uuid.UUID(str(value));return True
        except ValueError:
            return False

    def mapped_sequence(self, table, records):
        result=[]
        for r in records:
            old=str(r["id"])
            value=int.from_bytes(hashlib.sha256((table+":"+old).encode()).digest()[:8],"big") & (2**63-1)
            existing=self.target.execute("SELECT old_id FROM crm.legacy_id_map WHERE migration_id=%s AND kind=%s AND new_id=%s",(self.id,table,str(value))).fetchone()
            if existing and existing[0]!=old:
                raise RuntimeError("legacy sequence mapping collision")
            self.insert("legacy_id_map",[{"migration_id":str(self.id),"kind":table,"old_id":old,"new_id":str(value)}])
            result.append({**r,"id":value})
        return result

    def lookup(self, table, ids, schema="crm", key="id"):
        if not ids:
            return {}
        q=sql.SQL("SELECT {}::text,to_jsonb(t) FROM {}.{} t WHERE {}=ANY(%s::uuid[])").format(sql.Identifier(key),sql.Identifier(schema),sql.Identifier(table),sql.Identifier(key))
        return dict(self.target.execute(q,(list(ids),)))

    def analysis(self, kind, subject, record, output):
        # Imported generations are explicitly historical; they never enter a
        # runnable queue and retain their original input hash and model.
        job=str(uuid.uuid5(self.id,kind+":"+subject+":"+record["input_hash"]))
        policy={"include_messages":True,"include_private_notes":True}
        self.insert("ai_jobs",[{"id":job,"kind":kind,"subject":subject,"model":record.get("model") or "legacy","input_hash":record["input_hash"],"input":{},"source_versions":{"legacy_migration":str(self.id)},"privacy":{"policy":policy},"status":"complete","created_at":record["generated_at"],"completed_at":record["generated_at"]}])
        self.insert("ai_outputs",[{"kind":kind,"subject":subject,"output":output,"model":record.get("model") or "legacy","input_hash":record["input_hash"],"source_versions":{"legacy_migration":str(self.id)},"job_id":job,"generated_at":record["generated_at"]}])

    def queue_memory(self, kind, ids):
        if self.verifying or not ids:
            return
        self.target.execute("""WITH ids AS(SELECT DISTINCT unnest(%s::uuid[]) entity_id),owners AS(
 SELECT entity_id,profile_id FROM crm.memory_legacy_selected(%s,%s::uuid[]))
 INSERT INTO crm.memory_legacy_pending_records(kind,entity_id,profiles)
 SELECT %s,i.entity_id,coalesce(array_agg(o.profile_id ORDER BY o.profile_id) FILTER(WHERE o.profile_id IS NOT NULL),'{}'::uuid[])
 FROM ids i LEFT JOIN owners o USING(entity_id) GROUP BY i.entity_id ORDER BY i.entity_id
 ON CONFLICT(kind,entity_id) DO NOTHING""",(ids,kind,ids,kind))

    def archive(self, store, table, records, keys):
        values = []
        for r in records:
            k = canonical({key: r[key] for key in keys}) if keys else digest(r)
            # Session/service token hashes are not installed as live credentials.
            r = {key: value for key, value in r.items() if key not in {"token_hash", "secret", "password", "challenge"}}
            values.append({"migration_id": str(self.id), "store": store, "table_name": table,
                           "record_key": k, "payload": r})
        self.insert("legacy_history", values)

    def extra_metadata_spec(self,store,table):
        key=(store,table)
        if key not in self.source_metadata_spec:
            keys=primary_keys(self.sources[store],table)
            if store=="crm":
                # Content and message originals already have dedicated immutable
                # storage with complete source metadata and original UUIDs.
                spec=(None,keys) if table in TRANSFORMED-{"content_objects","message_events"} else ([],keys)
            else:
                covered={"payloads":{"sha256","value"},"objects":{"revision_number"},"revisions":{"object_id","revision_number","content_hash","observed_at","source_event"},"observations":{"object_id","source_event","content_hash","observed_at"}}
                mapped=set(columns(self.target,store,table))|covered.get(table,set())
                spec=(sorted(set(columns(self.sources[store],"public",table))-mapped),keys)
            self.source_metadata_spec[key]=spec
        return self.source_metadata_spec[key]

    def archive_extra_metadata(self,store,table,records):
        extra,keys=self.extra_metadata_spec(store,table)
        if extra is None:
            self.archive(store,table,[{k:v for k,v in r.items() if not k.startswith("__")} for r in records],keys)
        elif extra:
            self.archive(store,table,[{**{k:r[k] for k in set(extra+keys)},"__scope":"original_metadata"} for r in records],keys)

    def transform(self, store, table, records):
        self.archive_extra_metadata(store,table,records)
        if store != "crm":
            if table == "payloads":
                missing = [r for r in records if r["value"] is None and r.get("crm_content_kind")]
                resolved = {}
                for kind in {r["crm_content_kind"] for r in missing}:
                    hashes = [r["sha256"] for r in missing if r["crm_content_kind"] == kind]
                    resolved.update({(kind, h): v for h, v in self.sources["crm"].execute("SELECT sha256::text,payload_json FROM public.content_objects WHERE content_kind=%s AND sha256=ANY(%s::char(64)[])", (kind, hashes))})
                out = []
                for r in records:
                    value = r["value"]
                    if value is None and r.get("crm_content_kind"):
                        key = (r["crm_content_kind"], r["sha256"])
                        if key not in resolved:
                            raise RuntimeError("unresolved cross-store payload reference")
                        value = resolved[key]
                    out.append({"hash": r["sha256"], "value": value})
                self.insert("payloads", out, store)
            elif table == "objects":
                self.insert("objects", [{**r, "revision": r["revision_number"]} for r in records], store)
            elif table == "revisions":
                self.insert("revisions", [{"object_id": r["object_id"], "version": r["revision_number"], "hash": r["content_hash"]} for r in records], store)
                self.insert("revision_metadata", [{"object_id": r["object_id"], "version": r["revision_number"], "observed_at": r["observed_at"],"occurred_at":r.get("__occurred_at"),"source_event":r["source_event"]} for r in records], store)
            elif table == "observations":
                query=sql.SQL("SELECT jsonb_build_object('object_id',e.object_id,'event_id',e.source_event,'hash',e.content_hash,'version',v.version,'observed_at',e.observed_at) FROM jsonb_to_recordset(%s) e(object_id uuid,source_event text,content_hash text,observed_at timestamptz) LEFT JOIN LATERAL(SELECT r.version FROM {}.revisions r JOIN {}.revision_metadata m USING(object_id,version) WHERE r.object_id=e.object_id AND r.hash=e.content_hash ORDER BY (m.source_event=e.source_event) DESC,(m.observed_at<=e.observed_at) DESC,m.observed_at DESC,r.version DESC LIMIT 1) v ON true").format(sql.Identifier(store),sql.Identifier(store))
                out=[r[0] for r in self.target.execute(query,(Jsonb(records),))]
                if any(r["version"] is None for r in out):
                    raise RuntimeError("observation has no matching immutable revision")
                self.insert("observations", out, store)
            else:
                self.insert(table, records, store)
            return
        if table in DIRECT:
            # Keep original metadata that has no live Rust column. This includes
            # account-link evidence and historical OCR leases, never installing
            # a legacy lease or authentication session as executable state.
            if table not in {"group_memberships","profile_redirects"}:
                if table not in self.metadata_spec:
                    source_columns=set(columns(self.sources[store],"public",table))
                    target_columns=set(columns(self.target,"crm",DIRECT[table]))
                    self.metadata_spec[table]=(sorted(source_columns-target_columns),primary_keys(self.sources[store],table))
                extra,keys=self.metadata_spec[table]
                if extra:
                    self.archive(store,table,[{**{k:r[k] for k in set(extra+keys)},"__scope":"original_metadata"} for r in records],keys)
            if table == "app_users":
                for r in records:
                    if r.get("totp_secret"):
                        old = os.environ.get("MONICA_LEGACY_SESSION_SECRET")
                        new = os.environ.get("MONICA_SESSION_SECRET")
                        if not old or not new or old == new:
                            raise RuntimeError("distinct source/target encryption secrets required for TOTP")
                        old_value=Fernet(base64.urlsafe_b64encode(hashlib.sha256(old.encode()).digest())).decrypt(r["totp_secret"].encode())
                        cipher=Fernet(base64.urlsafe_b64encode(hashlib.sha256(new.encode()).digest()))
                        if self.verifying:
                            saved=self.target.execute("SELECT totp_secret FROM crm.app_users WHERE id=%s",(r["id"],)).fetchone()
                            if not saved or not saved[0] or not hmac.compare_digest(cipher.decrypt(saved[0].encode()),old_value):
                                raise RuntimeError("migrated two-factor secret mismatch")
                            r["totp_secret"]=saved[0]
                        else:
                            r["totp_secret"]=cipher.encrypt(old_value).decode()
            if table == "tag_memberships":
                self.insert("source_account_tags",[{"account_id":r["source_account_id"],"tag_id":r["tag_id"]} for r in records if r.get("source_account_id")])
                records=[r for r in records if r.get("source_account_id") is None]
                if not records:
                    return
            if table == "group_memberships":
                self.archive(store,table,records,["group_profile_id","person_profile_id"])
                # Source memberships follow account links through the source
                # table; copying their old person projection would pin owners.
                if self.source_memberships is None:
                    self.source_memberships={
                        (r["group_profile_id"],r["person_profile_id"]):(r["role"],r["joined_at"],r["left_at"])
                        for (r,) in self.sources["crm"].execute("SELECT jsonb_build_object('group_profile_id',g.profile_id,'person_profile_id',p.profile_id,'role',s.role,'joined_at',s.joined_at,'left_at',s.left_at) FROM public.source_group_memberships s JOIN public.source_account_links g ON g.account_id=s.group_account_id JOIN public.source_account_links p ON p.account_id=s.person_account_id")}
                records=[r for r in records if (r.get("role"),r.get("joined_at"),r.get("left_at")) != self.source_memberships.get((r["group_profile_id"],r["person_profile_id"]))]
                if not records:
                    return
                records=[{**r,"source_type":r.get("source_type","manual"),"source_record_id":str(uuid.UUID(r["source_record_id"])) if r.get("source_record_id") and self.is_uuid(r["source_record_id"]) else None} for r in records]
            if table == "faces":
                records = [{**r, "created_at": r["detected_at"]} for r in records]
            if table == "profiles":
                # A source may intentionally contain a blank display label;
                # keep the original in the snapshot, use its stable ID in UI.
                records=[{**r,"display_name":r["display_name"] or r["id"]} for r in records]
            if table == "activities":
                records=[{**r,"content_hash":digest(r),"updated_at":r["created_at"]} for r in records]
            if table == "profile_suggestion_decisions":
                records=[{**r,"created_at":r["decided_at"]} for r in records]
            if table == "profile_redirects":
                self.archive(store,table,records,["old_profile_id"])
                records=[{**r,"new_profile_id":r["profile_id"]} for r in records]
            self.insert(DIRECT[table], records)
            if table=="activity_participants":
                self.queue_memory("activity",list({r["activity_id"] for r in records}))
            return
        if table == "content_objects":
            self.insert(table, records)
            self.insert("legacy_content_locations",[{"content_object_id":r["id"],"encoding":r.get("encoding"),"storage_uri":r.get("storage_uri"),"created_at":r.get("created_at")} for r in records])
        elif table in ("companies", "schools"):
            self.insert("organizations", [{"id": self.organization_map[r["id"]], "kind": "company" if table == "companies" else "school", "name": r["name"], "normalized_name": r["normalized_name"]} for r in records])
        elif table == "profile_field_revisions":
            content = self.content(records)
            self.insert("field_revisions", [{**r, "value": content[r["content_object_id"]]} for r in records])
        elif table == "profile_field_current":
            # Source current values are installed separately with account ownership.
            self.insert("field_current", [{"profile_id": r["profile_id"], "field_key": r["field_key"], "revision_id": r["current_revision_id"]} for r in records if r["source_account_id"] is None])
        elif table == "source_account_fields":
            out = []
            for r in records:
                f = self.target.execute("SELECT value,content_hash,content_object_id,observed_at FROM crm.field_revisions WHERE id=%s", (r["revision_id"],)).fetchone()
                if not f:
                    raise RuntimeError("source field current pointer missing")
                out.append({"account_id": r["account_id"], "field_key": r["field_key"], "value": f[0], "content_hash": f[1], "content_object_id": str(f[2]), "observed_at": f[3].isoformat(), "object_id": str(uuid.uuid5(uuid.UUID(r["account_id"]), "legacy-field:"+r["field_key"])), "source_version": 0})
            self.insert("source_fields", out)
        elif table in ("profile_address_revisions","profile_employment_revisions","profile_education_revisions"):
            values=self.content(records)
            kind=table.removeprefix("profile_").removesuffix("_revisions")
            out=[]
            for r in records:
                owner=r.get("source_account_id") or r["profile_id"]
                entity=r.get("company_id") or r.get("school_id")
                value=values[r["content_object_id"]]
                out.append({**r,"revision_seq":r["revision_seq"]*3+{"address":0,"education":1,"employment":2}[kind],"fact_type":kind,"fact_key":r["fact_key"],"entity_id":self.organization_map.get(entity),"value":value,
                            "source_record_id":r.get("source_record_id") or "legacy:"+r["id"],
                            "object_id":str(uuid.uuid5(uuid.UUID(owner),"legacy-fact:"+kind+":"+r["fact_key"])),
                            "source_version":r["revision_seq"],"notebook_profile_id":r["profile_id"] if r.get("source_account_id") is None else None,
                            "position":value.get("position",0) if isinstance(value,dict) else 0})
            self.insert("source_structured_revisions",out)
        elif table == "profile_structured_current":
            self.insert("notebook_structured_current",[{"profile_id":r["profile_id"],"fact_type":r["fact_type"],"fact_key":r["fact_key"],"revision_id":r["current_revision_id"]} for r in records if r["source_account_id"] is None])
        elif table == "source_account_facts":
            self.insert("source_structured_current",[{"source_account_id":r["account_id"],"fact_type":r["fact_type"],"fact_key":r["fact_key"],"revision_id":r["revision_id"]} for r in records])
        elif table == "conversations":
            accounts=self.lookup("source_accounts",{r["source_account_id"] for r in records if r["source_account_id"]})
            out=[]
            for r in records:
                a=accounts.get(r["source_account_id"],{})
                provider=a.get("provider","wechat")
                account=(self.manifest["config"]["wechat_self_id"] if provider=="wechat" else None) or "legacy"
                out.append({**r,"provider":provider,"account":account,"external_id":r["external_id"] or r["id"],"message_count":0,"last_message_at":None,"last_message_preview":None})
            self.insert("conversations",out)
        elif table == "message_events":
            values=self.content(records)
            conversations=self.lookup("conversations",{r["conversation_id"] for r in records})
            senders=self.lookup("source_accounts",{r["sender_account_id"] for r in records if r["sender_account_id"]})
            out=[];evidence=[]
            for r in records:
                c=conversations[r["conversation_id"]];v=values[r["content_object_id"]]
                body=r.get("__body")
                if body is None:
                    body=v if isinstance(v,str) else v.get("text",v.get("body","")) if isinstance(v,dict) else canonical(v)
                sender=senders.get(r["sender_account_id"],{}).get("external_id") or (v.get("sender_external_id") if isinstance(v,dict) else None) or r["sender_profile_id"] or "unknown"
                # The legacy immutable identity includes conversation, source
                # record AND time. Preserve its UUID instead of collapsing edits.
                direction="unknown" if c["account"]=="legacy" or sender=="unknown" else "sent" if sender==c["account"] else "received"
                out.append({"message_id":r["id"],"conversation_id":r["conversation_id"],"provider":c["provider"],"account":c["account"],"external_id":r["id"],"sender":sender,"sender_account_id":r["sender_account_id"],"sender_profile_id":None if r["sender_account_id"] else r["sender_profile_id"],"occurred_at":r["occurred_at"],"body":body,"message_type":r["message_type"],"tombstone":False,"direction":direction})
                evidence.append({"message_id":r["id"],"source_record_id":r["source_record_id"],"content_object_id":r["content_object_id"],"raw_object_id":r["raw_object_id"],"original":{k:v for k,v in r.items() if not k.startswith("__")}})
            self.insert("message_search",out)
            self.insert("legacy_message_evidence",evidence)
            self.queue_memory("message",[r["id"] for r in records])
        elif table in ("social_posts","social_stories"):
            values=self.content(records)
            kind="post" if table=="social_posts" else "story"
            out=[]
            for r in records:
                owner=r["source_account_id"]
                oid=str(uuid.uuid5(uuid.UUID(owner),kind+":"+r["external_id"])) if owner else r["id"]
                content=values[r["content_object_id"]]
                out.append({**r,"object_id":oid,"event_type":kind,"content":content,"content_hash":r["current_content_hash"],"status":r.get("status",r.get("coverage_status","active")),"version":1})
            self.insert("timeline_events",out)
            self.queue_memory("social",[r["id"] for r in records])
        elif table in ("social_post_revisions","social_story_revisions"):
            self.insert("legacy_social_versions",[{**r,"event_id":r.get("post_id",r.get("story_id")),"original":r} for r in records])
        elif table == "social_interactions":
            values=self.content(records)
            self.insert("social_interactions",[{**r,"content":values.get(r["content_object_id"],{})} for r in records])
        elif table in ("tag_membership_revisions","relationship_edge_revisions"):
            self.insert(table,self.mapped_sequence(table,records))
        elif table == "activity_revisions":
            self.insert("activity_revisions",[{"id":r["sequence"],"activity_id":r["activity_id"],"operation":"import","payload":r["value"],"content_hash":digest(r["value"]),"created_at":r["recorded_at"]} for r in records])
        elif table in ("relationship_edges","source_relationship_edges"):
            types=dict(self.sources["crm"].execute("SELECT id::text,name FROM public.relationship_types"))
            self.insert(table,[{**r,"relationship_type":types[r["relationship_type_id"]],**({"legacy_source_owned":bool(r.get("from_account_id") or r.get("to_account_id"))} if table=="relationship_edges" else {})} for r in records])
            if table=="relationship_edges":
                self.insert("legacy_relationship_owners",[{"edge_id":r["id"],"from_account_id":r.get("from_account_id"),"to_account_id":r.get("to_account_id")} for r in records if r.get("from_account_id") or r.get("to_account_id")])
        elif table == "platform_link_decisions":
            self.insert(table,[{**r,"status":"confirmed" if r["status"]=="accepted" else r["status"],"account_id":r["source_account_id"],"profile_id":r["target_profile_id"],"actor_user_id":r["reviewed_by"],"updated_at":r["reviewed_at"]} for r in records])
            self.archive(store,table,records,["source_account_id","target_profile_id"])
        elif table == "source_contact_events":
            accounts=self.lookup("source_accounts",{r["account_id"] for r in records})
            links=dict(self.target.execute("SELECT account_id::text,profile_id::text FROM crm.source_account_links"))
            self.insert("contact_events",[{"id":str(uuid.uuid5(uuid.UUID(r["account_id"]),r["event_kind"]+":"+r["external_id"])),"provider":accounts[r["account_id"]]["provider"],"source_account_id":r["account_id"],"profile_id":links.get(r["account_id"]),"kind":r["event_kind"],"external_id":r["external_id"],"occurred_at":r["occurred_at"],"content":r["content"]} for r in records])
        elif table == "source_account_media":
            self.insert("media_slots",[{"entity_type":"source_account","entity_id":r["account_id"],"media_id":r["media_id"],"role":r["role"]} for r in records])
        elif table == "source_account_tags":
            self.insert("source_account_tags",records)
        elif table == "ai_outputs":
            kinds={"profile_memory":"ai.profile","profile_overview":"ai.profile_overview","profile_updates":"ai.profile_updates","community_name":"ai.community","reconnect":"ai.reconnect_one","profile_memory_chunk":"ai.profile_memory_chunk","profile_week_chunk":"ai.profile_week_chunk"}
            for r in records:
                if r["kind"] not in kinds:
                    raise RuntimeError("unknown legacy analysis kind")
                output=r["output"]
                if isinstance(output,dict):
                    output={**output,"legacy_kind":r["kind"]}
                    if r["kind"] in ("profile_memory","profile_overview") and "summary" not in output:
                        output["summary"]=output.get("overview",output.get("text",""))
                self.analysis(kinds[r["kind"]],r["subject"],r,output)
                if r["kind"]=="profile_updates":
                    self.insert("profile_suggestions",[{**item,"profile_id":r["subject"],"input_hash":r["input_hash"],"generated_at":r["generated_at"]} for item in r["output"].get("items",[])])
            self.archive(store,table,records,["kind","subject"])
        elif table == "person_weekly_updates":
            self.insert(table,records)
            for r in records:
                self.analysis("ai.person_week",r["profile_id"]+":"+r["week_start"],r,r["output"])
        elif table == "weekly_digests":
            for r in records:
                output={"facts":r["facts"],**(r["narrative"] if isinstance(r["narrative"],dict) else {"summary":r["narrative"]})}
                record={**r,"input_hash":digest(r["facts"])}
                self.insert("weekly_digests",[{"week_start":r["week_start"],"output":output,"input_hash":record["input_hash"],"generated_at":r["generated_at"]}])
                self.analysis("ai.digest",r["week_start"],record,output)
        elif table in ("social_post_media_readings","social_story_media_readings"):
            entity="post_id" if table=="social_post_media_readings" else "story_id"
            self.insert("media_readings",[{**r,"entity_id":r[entity],"entity_type":"social_post" if entity=="post_id" else "social_story","status":"complete"} for r in records])
        elif table == "reconnect_states":
            self.insert("ai_reconnect_decisions",[{"profile_id":r["profile_id"],"action":{"done":"contacted","snoozed":"snooze","dismissed":"dismiss"}[r["state"]],"until_at":r["until"],"created_at":r["updated_at"]} for r in records])
        else:
            raise RuntimeError("business transform not implemented: "+table)

    def transfer(self, store, entry):
        table, category = entry["table"], entry["mode"]
        if self.metadata_only and (category!="business" or self.extra_metadata_spec(store,table)[0]==[]):
            return
        saved = self.target.execute("SELECT processed,after_key,status FROM crm.legacy_migration_tables WHERE migration_id=%s AND store=%s AND table_name=%s", (self.id, store, table)).fetchone()
        processed, after, status = saved
        if self.verifying or self.metadata_only:
            processed,after,status=0,None,"verify"
        if status == "complete":
            return
        if category == "rebuild":
            if not self.verifying:
                self.target.execute("UPDATE crm.legacy_migration_tables SET processed=expected,status='complete' WHERE migration_id=%s AND store=%s AND table_name=%s", (self.id, store, table))
            return
        keys = entry["keys"]
        # Revision chains must precede their successors even when their UUIDs sort differently.
        if table in ("profile_field_revisions", "profile_address_revisions", "profile_employment_revisions", "profile_education_revisions"):
            keys = ["revision_seq"]
        while processed < entry["rows"]:
            if self.space_path and shutil.disk_usage(self.space_path).free<21*1024**3:
                raise RuntimeError("Mac free-space floor reached; resume committed batches later")
            attach_body=not self.metadata_only and store=="crm" and table=="message_events"
            attach_time=not self.metadata_only and store!="crm" and table=="revisions"
            query=sql.SQL("SELECT {} FROM public.{} t").format(sql.SQL("t.*") if attach_body or attach_time else sql.SQL("to_jsonb(t)"),sql.Identifier(table))
            params = []
            if keys:
                key_columns = sql.SQL(",").join(sql.SQL("t.{}").format(sql.Identifier(k)) for k in keys)
                if after is not None:
                    previous = sql.SQL(",").join(sql.SQL("(jsonb_populate_record(NULL::public.{},%s)).{}").format(sql.Identifier(table),sql.Identifier(k)) for k in keys)
                    query += sql.SQL(" WHERE ROW({})>ROW({})").format(key_columns,previous)
                    params += [Jsonb(after)]*len(keys)
                query += sql.SQL(" ORDER BY {} LIMIT 500").format(key_columns)
            else:
                query += sql.SQL(" ORDER BY ctid LIMIT 500 OFFSET %s")
                params.append(processed)
            # Limit source records BEFORE the indexed association. Otherwise a
            # merge join restarts the full message index for every UUID page.
            if attach_body:
                query=sql.SQL("WITH page AS MATERIALIZED({}) SELECT to_jsonb(t)||jsonb_build_object('__body',s.body) FROM page t LEFT JOIN LATERAL(SELECT body FROM public.message_search s WHERE s.message_id=t.id AND s.occurred_at=t.occurred_at LIMIT 1) s ON true ORDER BY {}").format(query,key_columns)
            if attach_time:
                query=sql.SQL("WITH page AS MATERIALIZED({}) SELECT to_jsonb(t)||jsonb_build_object('__occurred_at',o.occurred_at) FROM page t JOIN LATERAL(SELECT occurred_at FROM public.objects o WHERE o.id=t.object_id LIMIT 1) o ON true ORDER BY {}").format(query,key_columns)
            records = [r[0] for r in self.sources[store].execute(query,params)]
            if not records:
                raise RuntimeError("source changed or checkpoint inconsistent")
            with self.target.transaction():
                if self.metadata_only:
                    self.archive_extra_metadata(store,table,records)
                elif category == "history":
                    self.archive(store,table,records,keys)
                else:
                    self.transform(store,table,records)
                processed += len(records)
                after = {k: records[-1][k] for k in keys} if keys else None
                if not self.verifying and not self.metadata_only:
                    self.target.execute("UPDATE crm.legacy_migration_tables SET processed=%s,after_key=%s,status=%s WHERE migration_id=%s AND store=%s AND table_name=%s", (processed,Jsonb(after),"complete" if processed == entry["rows"] else "running",self.id,store,table))
                    self.target.execute("UPDATE crm.import_activity SET processed=(SELECT coalesce(sum(processed),0) FROM crm.legacy_migration_tables WHERE migration_id=%s AND mode<>'rebuild'),updated_at=now(),heartbeat_at=now() WHERE id=%s",(self.id,"migration:"+str(self.id)))
            print(json.dumps({"phase":"metadata_archive" if self.metadata_only else "verify" if self.verifying else "convert","store":store,"table":table,"processed":processed,"total":entry["rows"]}),flush=True)
        if not entry["rows"] and not self.verifying:
            self.target.execute("UPDATE crm.legacy_migration_tables SET status='complete' WHERE migration_id=%s AND store=%s AND table_name=%s", (self.id,store,table))

    def apply(self):
        self.guard_target(create=not self.verifying)
        # The ordering installs immutable content and referenced identities first.
        order = ["content_objects","media_assets","app_users","profiles","tags","companies","schools","source_accounts","source_account_links","identities","profile_field_revisions","profile_field_current","source_account_fields","profile_address_revisions","profile_employment_revisions","profile_education_revisions","profile_structured_current","source_account_facts","conversations","message_events","social_posts","social_stories","social_interactions","activities","activity_participants","relationship_edges"]
        for store in STORES:
            entries = self.manifest["stores"][store]["tables"]
            if store == "crm":
                entries = sorted(entries,key=lambda t:(order.index(t["table"]) if t["table"] in order else len(order),t["table"]))
            else:
                ranks = ["accounts","account_identifiers","payloads","objects","revisions","observations","media","media_links","migration_cursors"]
                entries = sorted(entries,key=lambda t:ranks.index(t["table"]))
            for entry in entries:
                self.transfer(store,entry)
        own=self.manifest["config"]["wechat_self_id"]
        if own and not self.verifying and not self.metadata_only:
            self.target.execute("INSERT INTO crm.person_identifiers(profile_id,kind,value,confirmed,is_self) SELECT l.profile_id,'source','wechat:'||a.external_id,true,true FROM crm.source_accounts a JOIN crm.source_account_links l ON l.account_id=a.id WHERE a.provider='wechat' AND a.external_id=%s ON CONFLICT(profile_id,kind,value) DO UPDATE SET is_self=true,confirmed=true",(own,))


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("action",choices=["inspect","apply","verify","metadata"])
    p.add_argument("--manifest",required=True,type=Path)
    args = p.parse_args()
    sources = {store:connect_source(store) for store in STORES}
    try:
        if args.action == "inspect":
            if args.manifest.exists():
                raise RuntimeError("manifest exists; use a new path")
            manifest=inspect_sources(sources)
            args.manifest.write_text(json.dumps(manifest,indent=2)+"\n")
            args.manifest.chmod(0o600)
            print(json.dumps({"phase":"inspected","manifest_hash":digest(manifest),"tables":sum(len(v["tables"]) for v in manifest["stores"].values())}))
            return
        manifest=json.loads(args.manifest.read_text())
        # Compare table identities and counts on copies; never trust a stale manifest.
        if manifest != inspect_sources(sources):
            raise RuntimeError("offline source copies changed after inspection")
        with psycopg.connect(os.environ["MONICA_DATABASE_URL"],autocommit=True) as target:
            importer=Importer(sources,target,manifest)
            importer.space_path=args.manifest.resolve().parent
            if args.action in {"apply","metadata"}:
                importer.metadata_only=args.action=="metadata"
                importer.apply()
            else:
                importer.guard_target(create=False)
                pending=target.execute("SELECT store,table_name FROM crm.legacy_migration_tables WHERE migration_id=%s AND (status<>'complete' OR processed<>expected)",(importer.id,)).fetchall()
                if pending:
                    raise RuntimeError("migration is incomplete: "+", ".join(s+"."+t for s,t in pending))
                importer.verifying=True
                importer.apply()
                print(json.dumps({"phase":"contents_verified","migration_id":str(importer.id)}))
    finally:
        for c in sources.values():
            c.close()


if __name__ == "__main__":
    try:
        main()
    except psycopg.Error as exc:
        print(json.dumps({"error":"database operation failed","sqlstate":exc.sqlstate,
                          "table":exc.diag.table_name,"column":exc.diag.column_name,
                          "constraint":exc.diag.constraint_name}),file=sys.stderr)
        sys.exit(1)
    except (RuntimeError,KeyError,ValueError) as exc:
        print(str(exc),file=sys.stderr)
        sys.exit(1)
