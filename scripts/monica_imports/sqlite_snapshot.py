"""Consistent standalone SQLite snapshots for a read-only source receiver.

Normal databases use a pinned SQLite read transaction. Fresh exported WAL files
without a readable index are captured into private storage first, where SQLite
can build its own coordination files. No source initialization daemon is needed.
"""
from contextlib import closing
import os
from pathlib import Path
import sqlite3
import tempfile
from urllib.parse import quote

from .files import canonical, fingerprint, sync_directory


class SourceChanged(ValueError):
    pass


def source_signature(source):
    result = []
    for suffix in ('', '-wal', '-journal'):
        path = Path(str(source) + suffix)
        if path.is_symlink():
            raise ValueError('sqlite_source_alias')
        result.append(list(fingerprint(canonical(path, independent=False))) if path.exists() else None)
    if result[0] is None:
        raise FileNotFoundError('sqlite_source_missing')
    return result


def copy_file(source, target, cancelled):
    with os.fdopen(os.open(source, os.O_RDONLY | os.O_NOFOLLOW), 'rb') as original, target.open('xb') as output:
        before = fingerprint(os.fstat(original.fileno()))
        while True:
            if cancelled():
                raise InterruptedError('sqlite_snapshot_cancelled')
            block = original.read(1024 * 1024)
            if not block:
                break
            output.write(block)
        output.flush()
        os.fsync(output.fileno())
        if before != fingerprint(os.fstat(original.fileno())):
            raise SourceChanged('sqlite_source_changed')
        return list(before)


def publish_snapshot(source, target, *, progress=None, cancelled=lambda: False):
    source, target = Path(source), Path(target)
    canonical(source, independent=False)
    canonical(target.parent, directory=True)
    if target.is_symlink():
        raise ValueError('sqlite_target_alias')
    if source == target or source.parent == target.parent:
        raise ValueError('sqlite_snapshot_requires_separate_storage')
    expected = source_signature(source)
    shm = Path(str(source) + '-shm')
    if shm.is_symlink():
        raise ValueError('sqlite_source_alias')
    # A live SQLite writer has a usable WAL index. Keep its normal MVCC read
    # protocol so appending commits never forces a complete file recapture.
    private_capture = expected[2] is not None or (expected[1] is not None and
                      (not shm.exists() or shm.stat().st_size < 32768))
    with tempfile.TemporaryDirectory(prefix='.sqlite-snapshot-', dir=target.parent) as folder:
        private = Path(folder) / 'source.db'
        if private_capture:
            for index, suffix in enumerate(('', '-wal', '-journal')):
                if expected[index] is not None:
                    copied = copy_file(Path(str(source) + suffix), Path(str(private) + suffix), cancelled)
                    if copied != expected[index]:
                        raise SourceChanged('sqlite_source_changed')
            if source_signature(source) != expected:
                raise SourceChanged('sqlite_source_changed')
            connection = sqlite3.connect(private)
        else:
            connection = sqlite3.connect('file:' + quote(str(source)) + '?mode=ro', uri=True, timeout=2)
        staged = Path(folder) / 'snapshot.db'
        def report(status, remaining, total):
            if cancelled():
                raise InterruptedError('sqlite_snapshot_cancelled')
            if progress:
                progress(status, remaining, total)
        with closing(connection) as reader, closing(sqlite3.connect(staged)) as output:
            # Recovery is permitted only in private storage. For a live source,
            # BEGIN precedes the first read and pins one committed snapshot.
            if private_capture:
                reader.execute('PRAGMA schema_version').fetchone()
            reader.execute('PRAGMA query_only=ON')
            reader.execute('BEGIN')
            try:
                reader.execute('PRAGMA schema_version').fetchone()
                reader.backup(output, pages=128, progress=report)
            finally:
                reader.rollback()
            if output.execute('PRAGMA quick_check').fetchall() != [('ok',)]:
                raise ValueError('sqlite_snapshot_invalid')
            output.execute('PRAGMA journal_mode=DELETE')
        with staged.open('rb') as result:
            os.fsync(result.fileno())
        os.replace(staged, target)
        sync_directory(target.parent)
    return expected
