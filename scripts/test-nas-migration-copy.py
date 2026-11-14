#!/usr/bin/env python3
"""Check that health cancellation cannot target a legacy or unrelated container."""
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec=importlib.util.spec_from_file_location("copy_controller",Path(__file__).with_name("nas-migration-copy.py"))
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)


class CancellationBoundary(unittest.TestCase):
    def test_mounted_evidence_inherits_read_only_service_group(self):
        with tempfile.TemporaryDirectory(prefix='copy-permissions-') as temporary:
            root=Path(temporary).resolve();source=root/'source';source.mkdir()
            data=root/'new';config=root/'config'
            nested=data/'snapshots/monica-next/media/objects/aa';nested.mkdir(parents=True);nested.chmod(0o700)
            (data/'migration').mkdir();(data/'migration/media.sqlite').touch()
            with patch.object(m,'DATA_ROOT',str(data)),patch.object(m,'CONFIG_ROOT',str(config)),patch.dict(m.SOURCES,{'media':(str(source),'monica-next/media',False)}),patch('os.chown') as chown:
                program=m.remote_program('media','synthetic-image','synthetic-copy',b'# synthetic worker')
                namespace={};exec(program.split('pause=root/')[0],namespace);namespace['lock'].close()
            self.assertEqual(data.stat().st_mode&0o7777,0o700)
            for path in [data/'snapshots',data/'snapshots/monica-next',data/'snapshots/monica-next/media',data/'snapshots/monica-next/media/objects',nested,data/'migration']:
                self.assertEqual(path.stat().st_mode&0o7777,0o2750)
                self.assertIn(__import__('unittest').mock.call(path,0,10001),chown.call_args_list)

    def test_missing_or_unrelated_container_is_never_stopped(self):
        for reply in [subprocess.CompletedProcess([],1,""),subprocess.CompletedProcess([],0,json.dumps([{"Id":"old-container","Config":{"Labels":{}}}]))]:
            with patch("subprocess.run",return_value=reply) as run,patch("pathlib.Path.write_text") as write:
                if reply.returncode:exec(m.stop_program("monica-rs-copy-synthetic"),{})
                else:
                    with self.assertRaisesRegex(RuntimeError,"unrelated"):exec(m.stop_program("monica-rs-copy-synthetic"),{})
                self.assertEqual(run.call_count,1)
                write.assert_not_called()

    def test_only_the_matching_new_worker_can_be_paused(self):
        name="monica-rs-copy-synthetic"
        reply=subprocess.CompletedProcess([],0,json.dumps([{"Id":"new-copy-id","Config":{"Labels":{"monica.migration.copy":name}}}]))
        with patch("subprocess.run",return_value=reply) as run,patch("pathlib.Path.write_text") as write:
            exec(m.stop_program(name),{})
            self.assertEqual(run.call_args.args[0][-1],"new-copy-id")
            self.assertIn("stop",run.call_args.args[0])
            write.assert_called_once()


if __name__=="__main__":unittest.main()
