#!/usr/bin/env python3
"""Capture real pinned FastAPI authentication validation without a database."""
import importlib.util
import json
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("legacy_contract", ROOT / "scripts/legacy-contract.py")
legacy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(legacy)
valid = {"email": "fixture@example.invalid", "password": "synthetic-password-only"}
csrf = {"cookie": "crm_csrf=synthetic", "x-csrf-token": "synthetic"}
cases = []
for route in ("bootstrap", "login"):
    for body in ({}, [], {"email": 123, "password": None},
                 {"email": "a", "password": "短" * 11},
                 {"email": "x" * 321, "password": "x" * 513}):
        cases.append(("POST", f"/api/v1/auth/{route}", body, {}, 422))
for code in ("12345", "123456789", 123456):
    cases.append(("POST", "/api/v1/auth/login", dict(valid, totp_code=code), {}, 422))
    cases.append(("POST", "/api/v1/auth/totp/verify", {"code": code}, csrf, 422))
cases.append(("POST", "/api/v1/auth/totp/verify", {}, csrf, 422))
legacy.RUNTIME = re.sub(r"requests = \[.*?\]\nfor method", "requests = " + repr(cases) + "\nfor method", legacy.RUNTIME, flags=re.S)
_, repository, commit = legacy.BASELINES[0]
result = legacy.runtime(repository, commit)
output = {"commit": commit, "dependencies": result["dependencies"], "fixtures": result["fixtures"]}
(ROOT / "docs/contracts/auth-validation.json").write_text(json.dumps(output, ensure_ascii=False, indent=2) + "\n")
print(f"Captured {len(cases)} real authentication validation cases")
