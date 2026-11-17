"""Correct empty-image model outputs without touching real posts or images."""
import importlib.util,json,uuid,unittest
from pathlib import Path
from psycopg.types.json import Jsonb
ROOT=Path(__file__).resolve().parents[1]
def load(name,path):
    spec=importlib.util.spec_from_file_location(name,ROOT/path);m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m);return m
contracts=load('empty_media_fixtures','scripts/test-nas-live-imports.py');repair=load('empty_media_correction','scripts/repair-empty-media-readings.py')
class EmptyImageCorrection(unittest.TestCase):
    @classmethod
    def setUpClass(cls):contracts.LiveImportContract.setUpClass.__func__(cls)
    def setUp(self):contracts.LiveImportContract.setUp(self)
    def test_bad_text_summary_is_audited_and_real_image_reading_survives(self):
        bad,good,media=uuid.uuid4(),uuid.uuid4(),uuid.uuid4()
        for eid,caption in[(bad,'Original text-only caption'),(good,'Original image caption')]:
            self.db.execute("INSERT INTO crm.timeline_events(id,provider,object_id,event_type,external_id,content,content_hash,version)VALUES(%s,'wechat',%s,'post',%s,%s,'original',1)",(eid,eid,str(eid),Jsonb(dict(text=caption))))
        self.db.execute("INSERT INTO crm.media_assets(id,sha256,media_type,byte_length,object_path)VALUES(%s,%s,'image/png',1,'retained-original')",(media,'a'*64))
        self.db.execute("INSERT INTO crm.media_links(entity_type,entity_id,media_id,role)VALUES('social_post',%s,%s,'media-1')",(good,media))
        for eid,evidence,summary in[(bad,[],'Incorrect OCR of caption'),(good,[dict(id=str(media),text='Real pixels')],'Real OCR')]:
            job=uuid.uuid4();self.db.execute("INSERT INTO crm.ai_jobs(id,kind,subject,model,input_hash,input,source_versions,privacy,status)VALUES(%s,'ai.post_media',%s,'fixture',%s,%s,'{}','{}','complete')",(job,str(eid),str(eid),Jsonb(dict(evidence=evidence))))
            self.db.execute("INSERT INTO crm.ai_subject_heads VALUES('ai.post_media',%s,%s)",(str(eid),job))
            self.db.execute("INSERT INTO crm.media_readings(entity_id,entity_type,status,summary,job_id)VALUES(%s,'social_post','complete',%s,%s)",(eid,summary,job))
            self.db.execute("INSERT INTO crm.ai_outputs(kind,subject,output,model,input_hash,source_versions,job_id)VALUES('ai.post_media',%s,%s,'fixture',%s,'{}',%s)",(str(eid),Jsonb(dict(summary=summary)),str(eid),job))
        result=repair.repair(self.db);self.assertEqual(result['archived_incorrect_readings'],1);self.assertEqual(result['cancelled_empty_evidence_jobs'],1)
        self.assertEqual(self.db.execute('SELECT entity_id,summary FROM crm.media_readings').fetchall(),[(good,'Real OCR')])
        self.assertEqual(self.db.execute('SELECT entity_id,media_id FROM crm.media_links').fetchall(),[(good,media)])
        self.assertEqual(self.db.execute("SELECT content->>'text'FROM crm.timeline_events WHERE id=%s",(bad,)).fetchone()[0],'Original text-only caption')
        original=self.db.execute("SELECT metadata FROM crm.audit_events WHERE action='ai.empty_image_evidence_corrected'").fetchone()[0];self.assertEqual(original['reading']['summary'],'Incorrect OCR of caption');self.assertEqual(original['output']['output']['summary'],'Incorrect OCR of caption')
        self.assertEqual(repair.repair(self.db)['archived_incorrect_readings'],0)
if __name__=='__main__':unittest.main()
