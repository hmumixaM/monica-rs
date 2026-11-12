#!/usr/bin/env python3
"""Export only pinned tracked backend sources for an isolated benchmark container.

No working-tree files, environment, archives, media, browser sessions or credentials
are read. The manifest is verified again inside ReferenceBackend before execution.
"""
import argparse,hashlib,json,subprocess
from pathlib import Path
from reference_backend import BASELINES

def stage(destination):
    destination=Path(destination).resolve()
    destination.mkdir(parents=True,exist_ok=False)
    manifest={"format":1,"baselines":{}}
    for name,(repository,commit) in BASELINES.items():
        paths=subprocess.check_output(["git","-C",repository,"ls-tree","-r","--name-only",commit,"services/api/app","migrations"]).decode().splitlines()
        paths=[p for p in paths if p.endswith((".py",".sql"))]
        paths += ["services/api/pyproject.toml","services/api/uv.lock"]
        files={}
        for path in paths:
            content=subprocess.check_output(["git","-C",repository,"show",f"{commit}:{path}"])
            target=destination/name/path
            target.parent.mkdir(parents=True,exist_ok=True)
            target.write_bytes(content)
            files[path]=hashlib.sha256(content).hexdigest()
        manifest["baselines"][name]={"repository":repository,"commit":commit,"files":files}
    (destination/"manifest.json").write_text(json.dumps(manifest,indent=2)+"\n")
    print(json.dumps({name:{"commit":value["commit"],"files":len(value["files"])} for name,value in manifest["baselines"].items()},indent=2))
if __name__=="__main__":
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument("destination");stage(parser.parse_args().destination)
