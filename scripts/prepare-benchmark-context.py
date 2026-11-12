#!/usr/bin/env python3
"""Create a source-only Docker benchmark context, never a copy of a worktree."""
import argparse,importlib.util,json,shutil,subprocess
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('destination');args=parser.parse_args()
root=Path(args.destination).resolve();root.mkdir(parents=True,exist_ok=False)
spec=importlib.util.spec_from_file_location('stage',ROOT/'scripts/stage-references.py');stage=importlib.util.module_from_spec(spec);spec.loader.exec_module(stage)
stage.stage(root/'references')
(root/'scripts').mkdir()
for filename in ['benchmark-backends.py','reference_backend.py']:
    shutil.copyfile(ROOT/'scripts'/filename,root/'scripts'/filename)
shutil.copyfile(ROOT/'infra/benchmark.Dockerfile',root/'Dockerfile')
(root/'source.json').write_text(json.dumps({'purpose':'synthetic isolated benchmark','source_worktrees_copied':False,'manifest':'references/manifest.json','rust_commit':subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip(),'rust_dirty':bool(subprocess.check_output(['git','status','--porcelain'],cwd=ROOT))},indent=2)+'\n')
print(root)
