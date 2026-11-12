#!/usr/bin/env python3
"""Regression for lost failure diagnostics; fake commands never start PostgreSQL."""
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

HERE=Path(__file__).resolve().parent
spec=importlib.util.spec_from_file_location('diagnostics',HERE/'test_diagnostics.py')
diagnostics=importlib.util.module_from_spec(spec);spec.loader.exec_module(diagnostics)

class RunnerFailures(unittest.TestCase):
    def test_setup_failure_retains_log_and_original_error_despite_cleanup_failure(self):
        with tempfile.TemporaryDirectory(prefix='monica-runner-failure-') as temporary:
            root=Path(temporary);pg=root/'fake-pg';pg.mkdir();artifacts=root/'artifacts'
            init=pg/'initdb';init.write_text(f'''#!{sys.executable}
from pathlib import Path
import sys
path=Path(sys.argv[sys.argv.index('-D')+1]);path.mkdir()
Path({str(root/'allocated-path')!r}).write_text(str(path.parent))
print('synthetic init failure marker',flush=True)
raise SystemExit(23)
''');init.chmod(0o700)
            stop=pg/'pg_ctl';stop.write_text(f'''#!{sys.executable}
from pathlib import Path
Path({str(root/'cleanup-attempted')!r}).write_text('owned cleanup attempted')
raise SystemExit(17)
''');stop.chmod(0o700)
            env={**os.environ,'MONICA_PG_BIN':str(pg),'MONICA_TEST_ARTIFACTS':str(artifacts)}
            for script in ['frontend-smoke.py','test-cargo.py']:
                with self.subTest(script=script):
                    result=subprocess.run([sys.executable,str(HERE/script)],env=env,capture_output=True,text=True)
                    self.assertNotEqual(result.returncode,0)
                    self.assertIn('exit status 23',result.stderr)
                    self.assertIn('Synthetic failure logs retained:',result.stderr)
                    self.assertTrue((root/'cleanup-attempted').exists())
                    self.assertFalse(Path((root/'allocated-path').read_text()).exists())
            logs=list(artifacts.glob('*/postgres.log'))
            self.assertGreaterEqual(len(logs),2)
            self.assertTrue(all('synthetic init failure marker' in path.read_text() for path in logs))

    def test_retained_logs_are_bounded_and_survive_source_cleanup(self):
        with tempfile.TemporaryDirectory(prefix='monica-runner-artifacts-') as artifacts:
            with patch.dict(os.environ,{'MONICA_TEST_ARTIFACTS':artifacts}):
                with tempfile.TemporaryDirectory(prefix='monica-runner-source-') as source:
                    payload=b'large synthetic log\n'*10000+b'failure tail marker'
                    Path(source,'server.log').write_bytes(payload)
                    saved=diagnostics.preserve_failure_logs(source,'fixture')
                self.assertFalse(Path(source).exists())
                self.assertEqual((saved/'server.log').read_bytes(),payload[-65536:])
                self.assertEqual(saved.stat().st_mode&0o777,0o700)

if __name__=='__main__':unittest.main()
