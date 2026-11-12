#!/usr/bin/env python3
"""Freeze pinned legacy contracts without opening a checkout's runtime/config.

Default/check modes need only Python's standard library. --runtime additionally
needs the pinned API dependencies and imports the real FastAPI app in a fresh,
network-denied subprocess; it never runs migrations or application workers.
"""
import argparse
import ast
from calendar import monthrange
from datetime import date, datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import random
import struct
import subprocess
import sys
import tempfile
import uuid

ROOT = Path(__file__).resolve().parents[1]
OUTPUT = ROOT / "docs/contracts"
BASELINES = (
    ("multisource", "/Users/maxhu/play/monica-multisource", "1a6e40e20c6ac12aca434b61244059d067847473"),
    ("crm", "/Users/maxhu/play/monica", "6d59cfef4b1a12f678d809a00e37d8332be70b06"),
    ("desktop", "/Users/maxhu/play/monica-desktop-public", "e35b49e4ca33406f0fdd7ecf8767931b44fdac87"),
)


def git(repository, *args):
    return subprocess.check_output(["git", "-C", repository, *args])


def source(repository, commit, path):
    return git(repository, "show", f"{commit}:{path}")


def pure_functions(repository, commit, path, names, constants=()):
    """Execute selected original pure definitions, not a rewritten oracle."""
    raw = source(repository, commit, path)
    tree = ast.parse(raw, filename=path)
    selected = []
    found = set()
    for node in tree.body:
        if isinstance(node, (ast.FunctionDef, ast.ClassDef)) and node.name in names:
            selected.append(node)
            found.add(node.name)
        elif isinstance(node, ast.Assign) and any(
            isinstance(t, ast.Name) and t.id in constants for t in node.targets
        ):
            selected.append(node)
    if found != set(names):
        raise RuntimeError(f"Missing original functions in {path}: {set(names) - found}")
    namespace = {"hashlib": hashlib, "json": json, "UUID": uuid.UUID,
                 "uuid5": uuid.uuid5, "Any": object, "date": date,
                 "datetime": datetime, "UTC": timezone.utc, "monthrange": monthrange}
    exec(compile(ast.Module(body=selected, type_ignores=[]), path, "exec"), namespace)
    return namespace, {"path": path, "sha256": hashlib.sha256(raw).hexdigest()}


def vectors(repository, commit):
    content, cp = pure_functions(repository, commit, "services/api/app/content.py",
                                 ("_json_safe", "canonical_json", "content_hash"))
    identity, ip = pure_functions(repository, commit, "services/api/app/platform_store.py",
                                  ("account_id", "object_id"), ("PROVIDERS", "NAMESPACE"))
    inputs = [None, True, False, 0, -1, 9223372036854775807,
              2**64, -(2**63)-1, 10**100, -(10**100), "", "0", "null",
              "张三😀", "line\n\t\"\\", "a\x00b",
              {"z": [1, None, "值\x00"], "a": {"b": False}},
              {"é": 1, "é": 2, "😀": 3, "𐀀": 4},
              {"ab": 1, "a\x00b": 2}, {"a\x00b": 2, "ab": 1},
              0.0, -0.0, 1.0, 1e-7, 1e-6, 1e-5, 0.0001, 1e15, 1e16,
              1e20, 1e23, 1.2345678901234567, 5e-324, 1.7976931348623157e308]
    rng = random.Random(20261111)
    # Finite binary64 values across the exponent range, not only readable decimal
    # examples. These catch shortest-round-trip/notation differences in Rust.
    for _ in range(128):
        bits = rng.getrandbits(64)
        if (bits >> 52) & 0x7ff != 0x7ff:
            inputs.append(struct.unpack(">d", bits.to_bytes(8, "big"))[0])
    cases = []
    for value in inputs:
        canonical = content["canonical_json"](value)
        cases.append({"input": value, "canonical_utf8": canonical.decode(),
                      "observation_sha256": hashlib.sha256(canonical).hexdigest(),
                      "content_sha256": content["content_hash"](value)})
    ids = []
    for provider in identity["PROVIDERS"]:
        for external in ("synthetic-account", "群_01@chatroom", "a:b", " leading "):
            aid = identity["account_id"](provider, external)
            for kind, oid in (("record", "synthetic-object"), ("post", "a:b"),
                              ("message", "0"), ("story", "合成😀")):
                ids.append({"provider": provider, "account": external, "kind": kind,
                            "external_id": oid, "account_id": str(aid),
                            "object_id": str(identity["object_id"](aid, kind, oid))})
    invalid = []
    for provider, external in (("unknown", "a"), ("wechat", ""), ("wechat", " \t\n")):
        try:
            identity["account_id"](provider, external)
        except ValueError as exc:
            invalid.append({"provider": provider, "account": external, "error": str(exc)})
        else:
            raise AssertionError("Invalid account accepted by baseline")
    lexical = []
    for raw in ("-0", "1e+003", "1.23456789012345678901234567890",
                "1e-999", '{"large":18446744073709551616,"small":1.0}'):
        parsed = json.loads(raw)
        encoded = content["canonical_json"](parsed)
        lexical.append({"input_json": raw, "canonical_utf8": encoded.decode(),
                        "observation_sha256": hashlib.sha256(encoded).hexdigest()})
    return {"commit": commit, "sources": [cp, ip], "canonical": cases, "lexical": lexical,
            "identities": ids, "invalid_accounts": invalid}


def declarations(repository, commit):
    """Declaration audit only; runtime registration is proven by --runtime."""
    routes = []
    paths = git(repository, "ls-tree", "-r", "--name-only", commit,
                "services/api/app").decode().splitlines()
    for path in paths:
        if not path.endswith(".py"):
            continue
        raw = source(repository, commit, path)
        tree = ast.parse(raw, filename=path)
        for node in ast.walk(tree):
            if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                continue
            for decorator in node.decorator_list:
                if not (isinstance(decorator, ast.Call)
                        and isinstance(decorator.func, ast.Attribute)
                        and isinstance(decorator.func.value, ast.Name)
                        and decorator.func.value.id in ("router", "app")
                        and decorator.func.attr in ("get", "post", "put", "patch", "delete", "head", "options")):
                    continue
                argument = decorator.args[0] if decorator.args else None
                if isinstance(argument, ast.Constant):
                    route_path = argument.value
                elif (path == "services/api/app/routers/mail_oauth.py"
                      and isinstance(argument, ast.Attribute)
                      and isinstance(argument.value, ast.Name)
                      and argument.value.id == "mail_oauth" and argument.attr == "CALLBACK"):
                    constants = ast.parse(source(repository, commit, "services/api/app/mail_oauth.py"))
                    route_path = next(ast.literal_eval(n.value) for n in constants.body
                                      if isinstance(n, ast.Assign) and any(isinstance(t, ast.Name) and t.id == "CALLBACK" for t in n.targets))
                else:
                    raise RuntimeError(f"Dynamic route declaration requires manual audit: {path}")
                routes.append({"method": decorator.func.attr.upper(),
                               "path": route_path, "source": path,
                               "handler": node.name, "line": decorator.lineno,
                               "source_sha256": hashlib.sha256(raw).hexdigest()})
    return {"commit": commit, "evidence": "declaration-only; not runtime registration",
            "routes": sorted(routes, key=lambda r: (r["path"], r["method"]))}


def date_vectors(repository, commit):
    original, provenance = pure_functions(repository, commit, "services/api/app/significant_dates.py",
        ("DateError", "normalize_date", "_part", "_validate_day", "_is_future"), ("KINDS", "PRECISIONS"))
    inputs = [
        {"kind":"birthday","precision":"month_day","month":2,"day":29,"label":"ignored"},
        {"kind":"birthday","precision":"exact","year":2000,"month":2,"day":29},
        {"kind":"birthday","precision":"exact","year":1900,"month":2,"day":29},
        {"kind":"birthday","precision":"exact","year":2026,"month":11,"day":12},
        {"kind":"birthday","precision":"approximate","year":2026},
        {"kind":"birthday","precision":"approximate","year":2027},
        {"kind":"other","precision":"month_day","year":2000,"month":2,"day":29},
        {"kind":"other","precision":"approximate","day":1},
        {"kind":"other","precision":"approximate","year":2000,"day":1},
        {"kind":"other","precision":"exact","year":2200,"month":1,"day":1,"label":" Future "},
        {"kind":"anniversary","precision":"approximate","year":2000,"month":2},
        {"kind":"other","precision":"month_day","month":4,"day":31},
        {"kind":"other","precision":"exact","year":1799,"month":1,"day":1},
        {"kind":"other","precision":"exact","year":2000,"month":0,"day":1},
        {"kind":"other","precision":"exact","year":2000,"month":1,"day":0},
        {"kind":"other","precision":"exact","year":2000,"month":1},
        {"kind":"other","precision":"month_day","month":1},
        {"kind":"bad","precision":"approximate","year":2000},
        {"kind":"other","precision":"bad","year":2000},
        {"kind":"other","precision":"approximate","month":1,"label":" "},
    ]
    cases=[]
    for payload in inputs:
        args={"label":None,"year":None,"month":None,"day":None,**payload}
        try:
            expected={"value":original["normalize_date"](**args,today=date(2026,11,11))}
        except ValueError as exc:
            expected={"error":str(exc)}
        cases.append({"input":payload,**expected})
    return {"commit":commit,"source":provenance,"today":[2026,11,11],"cases":cases}


def publish(name, value, check):
    payload = (json.dumps(value, ensure_ascii=False, indent=2) + "\n").encode()
    path = OUTPUT / name
    if check:
        if not path.exists() or path.read_bytes() != payload:
            raise RuntimeError(f"Frozen contract differs: {path}")
    else:
        OUTPUT.mkdir(parents=True, exist_ok=True)
        path.write_bytes(payload)


RUNTIME = r'''
import importlib.metadata, json, os, socket, sys
from pathlib import Path

def denied(*args, **kwargs):
    raise RuntimeError("Contract capture forbids database/network access")

socket.socket.connect = denied
socket.socket.connect_ex = denied
socket.create_connection = denied
import sqlalchemy.engine, psycopg
sqlalchemy.engine.Engine.connect = denied
psycopg.connect = denied
sys.path.insert(0, str(Path.cwd()))
from app.main import app
from app.auth import current_user
from app.db import get_db
from fastapi.testclient import TestClient

# These fixtures exercise routing, middleware and validation only. A DB access
# or accidental business-handler invocation is a hard capture failure.
app.dependency_overrides[get_db] = lambda: None
app.dependency_overrides[current_user] = lambda: {"id": "00000000-0000-0000-0000-000000000001"}
client = TestClient(app)
fixtures = []
requests = [
    ("GET", "/health", None, {}, 200),
    ("GET", "/api/v1/health", None, {}, 200),
    ("POST", "/api/v1/profiles", {"display_name":"Synthetic Person"}, {}, 403),
    ("POST", "/api/v1/profiles", {}, {"cookie":"crm_csrf=synthetic", "x-csrf-token":"synthetic"}, 422),
    ("POST", "/api/v1/profiles", {"display_name":""}, {"cookie":"crm_csrf=synthetic", "x-csrf-token":"synthetic"}, 422),
    ("GET", "/api/v1/profiles/not-a-uuid", None, {}, 422),
]
for method, path, body, headers, status in requests:
    response = client.request(method, path, json=body, headers=headers)
    assert response.status_code == status, (path, response.status_code, response.text)
    fixtures.append({"request":{"method":method,"path":path,"headers":headers,"json":body},
                     "response":{"status":status,"json":response.json()}})
packages = ("fastapi", "pydantic", "starlette", "sqlalchemy", "httpx")
Path("capture.json").write_text(json.dumps({"openapi":app.openapi(), "fixtures":fixtures,
    "dependencies":{name:importlib.metadata.version(name) for name in packages}}, ensure_ascii=False))
'''


def runtime(repository, commit):
    import tomllib

    with tempfile.TemporaryDirectory(prefix="monica-contract-") as tmp:
        root = Path(tmp)
        paths = git(repository, "ls-tree", "-r", "--name-only", commit,
                    "services/api/app").decode().splitlines()
        provenance = []
        for path in paths:
            if not path.endswith((".py", ".sql")):
                continue
            raw = source(repository, commit, path)
            target = root / Path(path).relative_to("services/api")
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(raw)
            provenance.append({"path": path, "sha256": hashlib.sha256(raw).hexdigest()})
        (root / "capture.py").write_text(RUNTIME)
        # No inherited application settings, tokens, PYTHONPATH, or .env.
        env = {"PATH": os.defpath, "PYTHONNOUSERSITE": "1", "PYTHONDONTWRITEBYTECODE": "1",
               "DATABASE_URL": "postgresql+psycopg://synthetic:synthetic@127.0.0.1:1/synthetic",
               "RAW_ROOT": str(root / "raw"), "MEDIA_ROOT": str(root / "media"),
               "AI_ENABLED": "false", "SESSION_SECRET": "synthetic-contract-only-secret",
               "IMPORT_HMAC_SECRET": "synthetic-contract-only-secret"}
        subprocess.run([sys.executable, "-I", str(root / "capture.py")],
                       cwd=root, env=env, check=True)
        result = json.loads((root / "capture.json").read_text())
        lock = source(repository, commit, "services/api/uv.lock")
        locked = tomllib.loads(lock.decode())
        for package, version in result["dependencies"].items():
            versions = {p["version"] for p in locked["package"] if p["name"] == package}
            if version not in versions:
                raise RuntimeError(f"{package}=={version} differs from {commit}'s lock: {versions}")
        result["commit"] = commit
        result["sources"] = provenance
        result["dependency_sources"] = [
            {"path": path, "sha256": hashlib.sha256(source(repository, commit, path)).hexdigest()}
            for path in ("services/api/pyproject.toml", "services/api/uv.lock")
        ]
        return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--runtime", action="store_true")
    parser.add_argument("--baseline", choices=("all", "crm", "desktop", "multisource"), default="all")
    args = parser.parse_args()
    for name, repository, commit in BASELINES:
        if args.baseline not in ("all", name):
            continue
        publish(f"{name}-golden.json", vectors(repository, commit), args.check)
        publish(f"{name}-declarations.json", declarations(repository, commit), args.check)
        publish(f"{name}-dates.json", date_vectors(repository, commit), args.check)
        if args.runtime:
            result = runtime(repository, commit)
            openapi = result.pop("openapi")
            operations = {(method.upper(), path) for path, item in openapi["paths"].items()
                          for method in item if method in ("get", "post", "put", "patch", "delete", "head", "options")}
            ledger = json.loads((ROOT / "docs/api-coverage.json").read_text())
            expected = {(r["method"], r["path"]) for r in ledger}
            result["ledger_missing_runtime_routes"] = sorted(operations - expected)
            result["ledger_routes_absent_at_runtime"] = sorted(expected - operations)
            publish(f"{name}-openapi.json", openapi, args.check)
            publish(f"{name}-http.json", result, args.check)
    print("Pinned legacy contracts " + ("verified" if args.check else "captured"))


if __name__ == "__main__":
    main()
