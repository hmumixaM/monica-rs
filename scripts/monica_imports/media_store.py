"""Content-addressed file publication, independent of migration/recovery jobs."""
import os
from pathlib import Path
import re
import shutil
import uuid

from .files import canonical, checksum, fingerprint, sync_directory

MARKER = 'monica-rust-isolated-media-v1\n'
_TEMPORARY = re.compile(r'(?:\.media-publish-[0-9a-f]{32}\.partial|\.source-recovery-[0-9a-f]{32})')


class MediaStore:
    def __init__(self, root):
        self.root = Path(root)
        info = canonical(self.root, directory=True)
        if (self.root / '.monica-rust-migration-media').read_text() != MARKER:
            raise ValueError('media_store_identity_invalid')
        self.owner = (info.st_uid, info.st_gid)

    def _published(self, target, digest, length):
        info = canonical(target, independent=False)
        if info.st_size != length or checksum(target, independent=False) != digest:
            raise ValueError('media_content_conflict')
        # Finish an interrupted no-replace publication only after accounting for
        # all same-directory temporary aliases. Unrelated hard links are errors.
        aliases = []
        if info.st_nlink > 1:
            for candidate in target.parent.iterdir():
                if not _TEMPORARY.fullmatch(candidate.name):
                    continue
                other = candidate.lstat()
                if (other.st_dev, other.st_ino) == (info.st_dev, info.st_ino):
                    canonical(candidate, independent=False)
                    aliases.append(candidate)
            if info.st_nlink != len(aliases) + 1:
                raise ValueError('media_has_unowned_hardlinks')
        with target.open('rb') as source:
            os.fsync(source.fileno())
        sync_directory(target.parent)
        for alias in aliases:
            alias.unlink()
        if aliases:
            sync_directory(target.parent)

    def publish(self, source, digest, length):
        source = Path(source)
        canonical(source)
        if not re.fullmatch('[0-9a-f]{64}', digest) or type(length) is not int or length <= 0:
            raise ValueError('media_identity_invalid')
        relative = Path('sha256') / digest[:2] / digest[2:4] / digest
        parent = self.root
        for component in relative.parts[:-1]:
            parent = parent / component
            try:
                parent.mkdir(mode=0o2750)
            except FileExistsError:
                canonical(parent, directory=True)
            else:
                os.chown(parent, *self.owner)
                parent.chmod(0o2750)
                sync_directory(parent.parent)
        target = self.root / relative
        if target.exists() or target.is_symlink():
            self._published(target, digest, length)
            return relative.as_posix()
        temporary = parent / ('.media-publish-' + uuid.uuid4().hex + '.partial')
        try:
            descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o640)
            with os.fdopen(descriptor, 'wb') as output, os.fdopen(os.open(source, os.O_RDONLY | os.O_NOFOLLOW), 'rb') as original:
                before = os.fstat(original.fileno())
                if before.st_size != length:
                    raise ValueError('media_size_changed')
                os.fchown(output.fileno(), *self.owner)
                shutil.copyfileobj(original, output, 1024 * 1024)
                output.flush()
                os.fsync(output.fileno())
                if fingerprint(before) != fingerprint(os.fstat(original.fileno())):
                    raise ValueError('media_source_changed')
            if checksum(temporary) != digest:
                raise ValueError('media_checksum_mismatch')
            try:
                os.link(temporary, target)
            except FileExistsError:
                pass
        finally:
            temporary.unlink(missing_ok=True)
        self._published(target, digest, length)
        return relative.as_posix()
