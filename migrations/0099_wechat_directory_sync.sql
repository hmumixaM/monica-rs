-- WeChat supplies a stable username independently of its editable alias.
CREATE TABLE crm.wechat_avatar_requests (
 account_id uuid PRIMARY KEY, native boolean NOT NULL, observed_at timestamptz NOT NULL,
 urls text[] NOT NULL, checksum text, token uuid NOT NULL DEFAULT gen_random_uuid(),
 next_attempt timestamptz, attempts integer NOT NULL DEFAULT 0
);
CREATE INDEX wechat_avatar_due ON crm.wechat_avatar_requests(next_attempt) WHERE next_attempt IS NOT NULL;

-- The subject is either a linked source account or the native connection's
-- contact evidence ID. Native contacts and relationship decisions stay scoped.
CREATE FUNCTION crm.queue_wechat_avatar(subject uuid, is_native boolean, urls text[], checksum text, observed timestamptz)
RETURNS void LANGUAGE sql AS $$
 INSERT INTO crm.wechat_avatar_requests AS q(account_id,native,urls,checksum,observed_at,next_attempt)
 VALUES(subject,is_native,urls,checksum,observed,now())
 ON CONFLICT(account_id) DO UPDATE SET urls=excluded.urls,checksum=excluded.checksum,
  observed_at=excluded.observed_at,next_attempt=now(),attempts=0,token=gen_random_uuid()
  WHERE (q.urls,q.checksum) IS DISTINCT FROM (excluded.urls,excluded.checksum) AND q.observed_at<=excluded.observed_at
$$;

CREATE FUNCTION crm.native_wechat_photo(person uuid) RETURNS uuid LANGUAGE sql STABLE AS $$
 SELECT s.media_id FROM crm.connection_evidence_people ep JOIN crm.connection_evidence e ON e.id=ep.evidence_id
 JOIN crm.media_slots s ON s.entity_type='wechat_native_contact' AND s.entity_id=e.id AND s.role='wechat_avatar'
 WHERE ep.profile_id=person AND ep.role='contact' AND NOT e.deleted
 AND NOT EXISTS(SELECT 1 FROM crm.media_slots m WHERE m.entity_type='profile' AND m.entity_id=person AND m.role='avatar')
 AND EXISTS(SELECT 1 FROM crm.profiles pf WHERE pf.id=person AND (pf.avatar_media_id IS NULL OR EXISTS(
   SELECT 1 FROM crm.source_avatar_projections pr WHERE pr.profile_id=pf.id AND pr.media_id=pf.avatar_media_id)))
 ORDER BY s.source_observed_at DESC NULLS LAST,s.updated_at DESC,s.entity_id LIMIT 1
$$;

-- Resolve memberships through the same current connection evidence as native
-- contact fields, so explicit reassignment and unlink are reflected on replay.
CREATE INDEX wechat_roster_evidence ON crm.connection_evidence(connection_id,external_id,revision) WHERE kind='group_membership' AND NOT deleted;
CREATE VIEW crm.native_wechat_memberships AS
 SELECT DISTINCT ON (g.profile_id,p.profile_id) g.profile_id group_profile_id,p.profile_id person_profile_id,
 NULL::text AS role,NULL::timestamptz AS joined_at,NULL::timestamptz AS left_at,
 r.observed_at,'wechat'::text AS source_type,e.id AS source_record_id
 FROM crm.connection_evidence e JOIN crm.connection_evidence_people g ON g.evidence_id=e.id AND g.role='room'
 JOIN crm.connection_evidence_people p ON p.evidence_id=e.id AND p.role='member'
 JOIN wechat.connection_revision_refs r ON (r.connection_id,r.external_id,r.revision)=(e.connection_id,e.external_id,e.revision)
 WHERE e.kind='group_membership' AND NOT e.deleted AND g.profile_id<>p.profile_id
 ORDER BY g.profile_id,p.profile_id,r.observed_at DESC,e.id;

CREATE FUNCTION crm.wechat_directory_account(external text, label text, kind text, candidate uuid, authoritative boolean DEFAULT false)
RETURNS uuid LANGUAGE plpgsql AS $$
DECLARE a uuid; owner uuid; prior_name text;
BEGIN
 SELECT id,display_name INTO a,prior_name FROM crm.source_accounts WHERE provider='wechat' AND external_id=external FOR UPDATE;
 IF a IS NULL THEN
   INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name)
   VALUES(candidate,'wechat',external,kind,coalesce(nullif(label,''),external))
   ON CONFLICT(provider,external_id) DO UPDATE SET external_id=excluded.external_id RETURNING id INTO a;
 END IF;
 -- The offline message importer reuses the same account in both schemas.
 INSERT INTO wechat.accounts(id,external_id,object_kind,display_name)
 VALUES(a,external,kind,coalesce(nullif(label,''),external)) ON CONFLICT DO NOTHING;
 SELECT profile_id INTO owner FROM crm.source_account_links WHERE account_id=a;
 IF owner IS NULL AND NOT EXISTS(SELECT 1 FROM crm.source_account_link_revisions WHERE account_id=a)
   AND NOT EXISTS(SELECT 1 FROM crm.platform_link_decisions WHERE account_id=a)
   AND NOT EXISTS(SELECT 1 FROM crm.identities WHERE (source_account_id=a OR (provider IN('wechat','wechat_group') AND external_id=external)) AND status IN('invalid','merged'))
 THEN
   SELECT profile_id INTO owner FROM crm.identities
   WHERE provider IN('wechat','wechat_group') AND external_id=external AND status IN('active','confirmed') AND profile_id IS NOT NULL
   ORDER BY (provider='wechat') DESC LIMIT 1;

   IF owner IS NULL THEN
     INSERT INTO crm.profiles(id,profile_type,display_name)
     VALUES(gen_random_uuid(),kind,left(coalesce(nullif(label,''),external),255)) RETURNING id INTO owner;
   END IF;
   INSERT INTO crm.source_account_links(account_id,profile_id) VALUES(a,owner);
 END IF;
 IF owner IS NOT NULL THEN
   INSERT INTO crm.identities AS i(id,profile_id,source_account_id,provider,external_id,display_name,status)
    VALUES(a,owner,a,CASE kind WHEN 'group' THEN 'wechat_group' ELSE 'wechat' END,external,coalesce(label,external),'active')
   ON CONFLICT(provider,external_id) DO UPDATE SET source_account_id=a
    WHERE i.profile_id=owner AND i.source_account_id IS NULL AND i.status NOT IN('invalid','merged');
 END IF;
 IF label IS NOT NULL AND (authoritative OR label<>external) AND label IS DISTINCT FROM prior_name THEN
   UPDATE crm.source_accounts SET display_name=label WHERE id=a;
   UPDATE crm.identities SET display_name=label WHERE source_account_id=a AND display_name IS DISTINCT FROM label;
   UPDATE crm.profiles SET display_name=left(label,255),generation=generation+1
    WHERE id=owner AND display_name IN(external,prior_name) AND archived_at IS NULL;
 END IF;
 RETURN a;
END $$;

CREATE FUNCTION crm.wechat_directory_contact(external text, label text, fields jsonb, checksum text, candidate uuid)
RETURNS void LANGUAGE plpgsql AS $$
DECLARE a uuid; owner uuid; k text; v jsonb; h text; old crm.field_revisions; source_old crm.source_fields;
        revision uuid; urls text[];
BEGIN
 IF crm.wechat_is_official(external) THEN RETURN; END IF;
 a=crm.wechat_directory_account(external,label,'person',candidate,true);
 SELECT p.id INTO owner FROM crm.source_account_links l JOIN crm.profiles p ON p.id=l.profile_id
 WHERE l.account_id=a AND p.archived_at IS NULL FOR UPDATE OF p;
 IF fields ? 'big_head_url' OR fields ? 'small_head_url' THEN
   urls=array_remove(ARRAY[nullif(fields->>'big_head_url',''),nullif(fields->>'small_head_url','')],NULL);
   fields=(fields-'big_head_url'-'small_head_url')||jsonb_build_object('avatar_url',urls[1]);
   PERFORM crm.queue_wechat_avatar(a,false,urls,checksum,now());
 END IF;
 IF fields ? 'alias' THEN
   UPDATE crm.source_accounts SET username=fields->>'alias' WHERE id=a AND username IS DISTINCT FROM fields->>'alias';
   UPDATE wechat.accounts SET username=fields->>'alias' WHERE id=a AND username IS DISTINCT FROM fields->>'alias';
   UPDATE crm.identities SET username=fields->>'alias' WHERE source_account_id=a AND username IS DISTINCT FROM fields->>'alias';
 END IF;
 FOR k,v IN SELECT 'wechat.'||CASE key WHEN 'nick_name' THEN 'nickname' ELSE key END,value
   FROM jsonb_each(fields) WHERE key IN('alias','remark','nick_name','description','signature','country','province','city','region','labels','add_source','moments_cover_url','avatar_url')
   ORDER BY key
 LOOP
   SELECT * INTO source_old FROM crm.source_fields WHERE account_id=a AND field_key=k;
   SELECT r.* INTO old FROM crm.field_current c JOIN crm.field_revisions r ON r.id=c.revision_id
    WHERE c.profile_id=owner AND c.field_key=k;
   IF source_old.value IS NOT DISTINCT FROM v OR (source_old.account_id IS NULL AND old.id IS NULL AND v='null') THEN CONTINUE; END IF;
   IF jsonb_typeof(v)='array' THEN
     SELECT encode(sha256(convert_to('['||coalesce(string_agg(element::text,',' ORDER BY ordinal),'')||']','UTF8')),'hex')
     INTO h FROM jsonb_array_elements(v) WITH ORDINALITY AS items(element,ordinal);
   ELSE
     h=encode(sha256(convert_to(coalesce(v#>>'{}','null'),'UTF8')),'hex');
   END IF;
   INSERT INTO crm.source_fields AS f(account_id,field_key,value,content_hash,object_id,source_version)
    VALUES(a,k,v,h,a,1) ON CONFLICT(account_id,field_key) DO UPDATE SET value=v,content_hash=h,object_id=a,
    source_version=f.source_version+1,observed_at=now(),content_object_id=NULL;
   IF old.value IS NOT DISTINCT FROM v AND source_old.account_id IS NULL THEN CONTINUE; END IF;
   INSERT INTO crm.field_revisions(id,profile_id,field_key,value,content_hash,source_type,operation,
     previous_revision_id,source_account_id,is_conflict)
    VALUES(gen_random_uuid(),owner,k,v,h,'wechat','import',old.id,a,coalesce(old.source_type IN('manual','monica'),false))
    RETURNING id INTO revision;
   IF owner IS NOT NULL AND (old.source_type IS NULL OR old.source_type NOT IN('manual','monica')) THEN
     INSERT INTO crm.field_current VALUES(owner,k,revision) ON CONFLICT(profile_id,field_key) DO UPDATE SET revision_id=excluded.revision_id;
     INSERT INTO crm.memory_profile_refresh VALUES(owner) ON CONFLICT DO NOTHING;
   END IF;
 END LOOP;
END $$;

CREATE FUNCTION crm.wechat_directory_member(room text,room_label text,member text,member_label text,room_candidate uuid,member_candidate uuid)
RETURNS void LANGUAGE plpgsql AS $$
DECLARE g uuid; p uuid;
BEGIN
 IF right(rtrim(room),9)<>'@chatroom' OR member='' OR right(rtrim(member),9)='@chatroom' THEN RETURN; END IF;
 g=crm.wechat_directory_account(room,room_label,'group',room_candidate);
 p=crm.wechat_directory_account(member,member_label,'person',member_candidate);
 INSERT INTO crm.source_group_memberships AS m(group_account_id,person_account_id,role)
 VALUES(g,p,NULL) ON CONFLICT(group_account_id,person_account_id) DO UPDATE SET left_at=NULL WHERE m.left_at IS NOT NULL;
END $$;

CREATE OR REPLACE VIEW crm.current_group_memberships AS
 WITH existing AS NOT MATERIALIZED ( SELECT * FROM crm.group_memberships
 UNION ALL
 SELECT gl.profile_id,pl.profile_id,s.role,s.joined_at,s.left_at,
   greatest(gl.linked_at,pl.linked_at),'wechat',s.group_account_id
 FROM crm.source_group_memberships s
 JOIN crm.source_account_links gl ON gl.account_id=s.group_account_id
 JOIN crm.source_account_links pl ON pl.account_id=s.person_account_id
 WHERE gl.profile_id<>pl.profile_id AND NOT EXISTS(
   SELECT 1 FROM crm.group_memberships m WHERE m.group_profile_id=gl.profile_id AND m.person_profile_id=pl.profile_id))
 SELECT * FROM existing UNION ALL SELECT n.* FROM crm.native_wechat_memberships n
 WHERE NOT EXISTS(SELECT 1 FROM existing e WHERE (e.group_profile_id,e.person_profile_id)=(n.group_profile_id,n.person_profile_id));
