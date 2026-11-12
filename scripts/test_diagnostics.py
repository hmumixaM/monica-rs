"""Preserve bounded synthetic test diagnostics without changing test exit status."""
import os
from pathlib import Path
import sys
import tempfile


def preserve_failure_logs(source, label, names=('server.log', 'postgres.log'), limit=65536):
    source=Path(source)
    try:
        available=[name for name in names if (source/name).is_file()]
        if not available:
            return None
        base=Path(os.environ.get('MONICA_TEST_ARTIFACTS',str(Path(__file__).resolve().parents[1]/'.local-validation/test-failures')))
        base.mkdir(parents=True,exist_ok=True)
        destination=Path(tempfile.mkdtemp(prefix=label+'-',dir=base))
        for name in available:
            with (source/name).open('rb') as stream:
                stream.seek(0,2)
                stream.seek(max(0,stream.tell()-limit))
                (destination/name).write_bytes(stream.read(limit))
        print('Synthetic failure logs retained: '+str(destination),file=sys.stderr,flush=True)
        return destination
    except OSError as error:
        print('Could not retain synthetic test diagnostics: '+str(error),file=sys.stderr)
        return None
