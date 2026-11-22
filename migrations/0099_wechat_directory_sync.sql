-- WeChat supplies a stable username independently of its editable alias.
CREATE TABLE crm.wechat_avatar_requests (
 account_id uuid PRIMARY KEY REFERENCES crm.source_accounts(id),
 urls text[] NOT NULL, checksum text, token uuid NOT NULL DEFAULT gen_random_uuid(),
 next_attempt timestamptz, attempts integer NOT NULL DEFAULT 0
);
CREATE INDEX wechat_avatar_due ON crm.wechat_avatar_requests(next_attempt) WHERE next_attempt IS NOT NULL;

CREATE FUNCTION crm.wechat_directory_account(external text, label text, kind text, candidate uuid, preferred uuid DEFAULT NULL)
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
   AND NOT EXISTS(SELECT 1 FROM crm.identities WHERE source_account_id=a AND status IN('invalid','merged'))
 THEN
   SELECT profile_id INTO owner FROM crm.identities
   WHERE provider IN('wechat','wechat_group') AND external_id=external AND status IN('active','confirmed') AND profile_id IS NOT NULL
   ORDER BY (provider='wechat') DESC LIMIT 1;
   owner=coalesce(owner,preferred);
   IF owner IS NULL THEN
     INSERT INTO crm.profiles(id,profile_type,display_name)
     VALUES(gen_random_uuid(),kind,left(coalesce(nullif(label,''),external),255)) RETURNING id INTO owner;
   END IF;
   INSERT INTO crm.source_account_links(account_id,profile_id) VALUES(a,owner);
 END IF;
 IF label IS NOT NULL AND label<>external AND label IS DISTINCT FROM prior_name THEN
   UPDATE crm.source_accounts SET display_name=label WHERE id=a;
   UPDATE crm.profiles SET display_name=left(label,255),generation=generation+1
    WHERE id=owner AND display_name IN(external,prior_name) AND archived_at IS NULL;
 END IF;
 RETURN a;
END $$;

CREATE FUNCTION crm.wechat_directory_contact(external text, label text, fields jsonb, checksum text, candidate uuid, preferred uuid DEFAULT NULL)
RETURNS void LANGUAGE plpgsql AS $$
DECLARE a uuid; owner uuid; k text; v jsonb; h text; old crm.field_revisions; source_old crm.source_fields;
        revision uuid; urls text[];
BEGIN
 IF crm.wechat_is_official(external) THEN RETURN; END IF;
 a=crm.wechat_directory_account(external,label,'person',candidate,preferred);
 SELECT p.id INTO owner FROM crm.source_account_links l JOIN crm.profiles p ON p.id=l.profile_id
 WHERE l.account_id=a AND p.archived_at IS NULL FOR UPDATE OF p;
 IF fields ? 'big_head_url' OR fields ? 'small_head_url' THEN
   urls=array_remove(ARRAY[nullif(fields->>'big_head_url',''),nullif(fields->>'small_head_url','')],NULL);
   fields=(fields-'big_head_url'-'small_head_url')||jsonb_build_object('avatar_url',urls[1]);
   INSERT INTO crm.wechat_avatar_requests AS q(account_id,urls,checksum,next_attempt)
   VALUES(a,urls,checksum,CASE WHEN cardinality(urls)>0 THEN now() END)
   ON CONFLICT(account_id) DO UPDATE SET urls=excluded.urls,checksum=excluded.checksum,
    next_attempt=excluded.next_attempt,attempts=0,token=gen_random_uuid()
    WHERE (q.urls,q.checksum) IS DISTINCT FROM (excluded.urls,excluded.checksum);
 END IF;
 IF fields ? 'alias' THEN
   UPDATE crm.source_accounts SET username=fields->>'alias' WHERE id=a AND username IS DISTINCT FROM fields->>'alias';
   UPDATE crm.identities SET username=fields->>'alias' WHERE source_account_id=a AND username IS DISTINCT FROM fields->>'alias';
 END IF;
 FOR k,v IN SELECT 'wechat.'||CASE key WHEN 'nick_name' THEN 'nickname' ELSE key END,value
   FROM jsonb_each(fields) WHERE key IN('alias','remark','nick_name','description','signature','country','province','city','avatar_url')
   ORDER BY key
 LOOP
   SELECT * INTO source_old FROM crm.source_fields WHERE account_id=a AND field_key=k;
   SELECT r.* INTO old FROM crm.field_current c JOIN crm.field_revisions r ON r.id=c.revision_id
    WHERE c.profile_id=owner AND c.field_key=k;
   IF source_old.value IS NOT DISTINCT FROM v OR (source_old.account_id IS NULL AND old.id IS NULL AND v='null') THEN CONTINUE; END IF;
   h=encode(sha256(convert_to(coalesce(v#>>'{}','null'),'UTF8')),'hex');
   INSERT INTO crm.source_fields AS f(account_id,field_key,value,content_hash,object_id,source_version)
    VALUES(a,k,v,h,a,1) ON CONFLICT(account_id,field_key) DO UPDATE SET value=v,content_hash=h,object_id=a,
    source_version=f.source_version+1,observed_at=now(),content_object_id=NULL;
   IF owner IS NULL OR old.value IS NOT DISTINCT FROM v THEN CONTINUE; END IF;
   INSERT INTO crm.field_revisions(id,profile_id,field_key,value,content_hash,source_type,operation,
     previous_revision_id,source_account_id,is_conflict)
    VALUES(gen_random_uuid(),owner,k,v,h,'wechat','import',old.id,a,coalesce(old.source_type IN('manual','monica'),false))
    RETURNING id INTO revision;
   IF old.source_type IS NULL OR old.source_type NOT IN('manual','monica') THEN
     INSERT INTO crm.field_current VALUES(owner,k,revision) ON CONFLICT(profile_id,field_key) DO UPDATE SET revision_id=excluded.revision_id;
   END IF;
 END LOOP;
END $$;

CREATE FUNCTION crm.wechat_directory_member(room text,room_label text,member text,member_label text,room_candidate uuid,member_candidate uuid,preferred uuid DEFAULT NULL)
RETURNS void LANGUAGE plpgsql AS $$
DECLARE g uuid; p uuid;
BEGIN
 IF right(rtrim(room),9)<>'@chatroom' OR member='' OR right(rtrim(member),9)='@chatroom' THEN RETURN; END IF;
 g=crm.wechat_directory_account(room,room_label,'group',room_candidate);
 p=crm.wechat_directory_account(member,member_label,'person',member_candidate,preferred);
 INSERT INTO crm.source_group_memberships AS m(group_account_id,person_account_id,role)
 VALUES(g,p,'member') ON CONFLICT(group_account_id,person_account_id) DO UPDATE SET left_at=NULL WHERE m.left_at IS NOT NULL;
END $$;
