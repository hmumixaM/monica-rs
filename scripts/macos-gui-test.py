#!/usr/bin/env python3
"""Prepare a small debug .app with an isolated synthetic data directory for UI acceptance.

Launch the printed .app through the native UI. This wrapper guarantees that a
LaunchServices restart retains the isolated paths. It does not start collectors,
change login items, or copy a database. Build the debug custom-protocol shell first.
"""
import argparse
import json
import plistlib
import shlex
import shutil
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--server', type=Path, default=ROOT/'target/debug/monica-server')
p.add_argument('--postgres', type=Path, default=ROOT/'apps/native-desktop/runtime/postgres')
p.add_argument('--shell', type=Path, default=ROOT/'apps/native-desktop/src-tauri/target/debug/monica-rust-desktop')
a = p.parse_args()
for item in [a.server, a.shell, a.postgres/'bin/postgres', ROOT/'apps/web/dist/index.html']:
    if not item.is_file(): p.error(f'Build the explicit test input first: {item}')
root = Path(tempfile.mkdtemp(prefix='monica-rust-gui-'))
runtime = root/'runtime'
runtime.mkdir()
(runtime/'postgres').symlink_to(a.postgres.resolve(), target_is_directory=True)
(runtime/'monica-server').symlink_to(a.server.resolve())
(runtime/'web').symlink_to(ROOT/'apps/web/dist', target_is_directory=True)
(runtime/'tools').mkdir()
(runtime/'tools/bin').symlink_to('/opt/homebrew/bin', target_is_directory=True)
app = root/'Monica Rust GUI Test.app'
(app/'Contents/MacOS').mkdir(parents=True)
(app/'Contents/Resources').mkdir()
shutil.copy2(a.shell, app/'Contents/MacOS/monica-rust-desktop')
with (app/'Contents/Info.plist').open('wb') as stream:
    plistlib.dump({'CFBundleExecutable':'launch-isolated','CFBundleIdentifier':'org.monica.rust.desktop',
                  'CFBundleName':'Monica Rust GUI Test','CFBundlePackageType':'APPL','LSUIElement':True},stream)
launcher = app/'Contents/MacOS/launch-isolated'
launcher.write_text('#!/bin/sh\nexport MONICA_RUST_RUNTIME='+shlex.quote(str(runtime))+
                    '\nexport MONICA_RUST_TEST_DATA_DIR='+shlex.quote(str(root/'data'))+
                    '\nexec '+shlex.quote(str(app/'Contents/MacOS/monica-rust-desktop'))+'\n')
launcher.chmod(0o755)
print(json.dumps({'app':str(app),'data':str(root/'data'),'runtime':str(runtime)},indent=2))
