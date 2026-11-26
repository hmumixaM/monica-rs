#!/usr/bin/env python3
"""Build the pinned, relocatable PostgreSQL runtime for Monica Rust on macOS."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


POSTGRES_SHA256 = 'c1575341fa7bd40f5274ea465b34390f4dc64cdd0770af327005caaeb9f6b7ed'
HOMEBREW = Path('/opt/homebrew/opt')
DEPLOYMENT_TARGET = '26.0'
EXTENSIONS = ('pg_trgm', 'pg_stat_statements')


def build(archive, output, jobs):
    if hashlib.sha256(archive.read_bytes()).hexdigest() != POSTGRES_SHA256:
        raise ValueError('expected the upstream PostgreSQL 16.15 source archive')
    dependencies = ('openssl@3', 'lz4', 'zstd')
    arguments = [
        f'--prefix={output}', '--without-readline',
        '--with-openssl', '--with-icu', '--with-lz4', '--with-zstd',
    ]
    env = {
        **os.environ,
        'MACOSX_DEPLOYMENT_TARGET': DEPLOYMENT_TARGET,
        'CFLAGS': '-O2',
        'CPPFLAGS': ' '.join(f'-I{HOMEBREW / name / "include"}' for name in dependencies),
        'LDFLAGS': ' '.join(f'-L{HOMEBREW / name / "lib"}' for name in dependencies),
        'PKG_CONFIG_PATH': ':'.join(
            str(HOMEBREW / name / 'lib/pkgconfig') for name in (*dependencies, 'icu4c@78')
        ),
    }
    with tempfile.TemporaryDirectory(prefix='monica-build-postgres-') as folder:
        source = Path(folder)
        subprocess.run(['/usr/bin/tar', '-xf', archive, '--strip-components=1', '-C', source], check=True)
        subprocess.run(['./configure', *arguments], cwd=source, env=env, check=True)
        subprocess.run(['make', '-j', str(jobs)], cwd=source, env=env, check=True)
        subprocess.run(['make', 'install'], cwd=source, env=env, check=True)
        for extension in EXTENSIONS:
            subprocess.run(
                ['make', '-C', f'contrib/{extension}', '-j', str(jobs), 'all', 'install'],
                cwd=source, env=env, check=True,
            )
        notices = output / 'monica-licenses'
        notices.mkdir()
        shutil.copy2(source / 'COPYRIGHT', notices / 'COPYRIGHT')
        build = {'archive': archive.name, 'sha256': POSTGRES_SHA256, 'configure': arguments,
                 'deployment_target': DEPLOYMENT_TARGET, 'extensions': EXTENSIONS}
        (notices / 'BUILD.json').write_text(json.dumps(build, indent=2) + '\n')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source', type=Path)
    parser.add_argument('output', type=Path)
    parser.add_argument('--jobs', type=int, default=6)
    args = parser.parse_args()
    output = args.output.resolve()
    if output.exists() or args.jobs < 1:
        parser.error('output must be absent and jobs must be positive')
    build(args.source.resolve(strict=True), output, args.jobs)


if __name__ == '__main__':
    main()
