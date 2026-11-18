"""Durable file operations shared by import storage and snapshot publication."""
import hashlib
import json
import os
from pathlib import Path
import stat
import tempfile


def canonical(path, *, directory=False, independent=True):
    path = Path(path)
    if not path.is_absolute() or path.resolve(strict=True) != path:
        raise ValueError('file_path_alias')
    info = path.stat()
    valid = stat.S_ISDIR(info.st_mode) if directory else stat.S_ISREG(info.st_mode)
    if not valid or (not directory and independent and info.st_nlink != 1):
        raise ValueError('file_identity_invalid')
    return info


def fingerprint(info):
    return (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, info.st_ctime_ns)


def checksum(path, *, independent=True):
    canonical(path, independent=independent)
    with os.fdopen(os.open(path, os.O_RDONLY | os.O_NOFOLLOW), 'rb') as source:
        before = os.fstat(source.fileno())
        result = hashlib.sha256()
        for block in iter(lambda: source.read(1024 * 1024), b''):
            result.update(block)
        if fingerprint(before) != fingerprint(os.fstat(source.fileno())):
            raise ValueError('file_changed_during_read')
    return result.hexdigest()


def sync_directory(path):
    descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def atomic_json(path, value):
    path = Path(path)
    canonical(path.parent, directory=True)
    if path.is_symlink():
        raise ValueError('receipt_path_alias')
    descriptor, name = tempfile.mkstemp(prefix='.' + path.name + '-', dir=path.parent)
    try:
        with os.fdopen(descriptor, 'w') as output:
            json.dump(value, output, sort_keys=True)
            output.flush()
            os.fsync(output.fileno())
        os.replace(name, path)
        sync_directory(path.parent)
    finally:
        Path(name).unlink(missing_ok=True)
