#!/usr/bin/env python3
"""Execute the same real authentication lifecycle against pinned Python and Rust."""
import argparse
import base64
import hashlib
import hmac
import http.cookiejar
import json
import os
from pathlib import Path
import re
import signal
import socket
import struct
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request
from reference_backend import PG, ReferenceBackend


class Client:
    def __init__(self, base):
        self.base = base
        self.jar = http.cookiejar.CookieJar()
        self.opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(self.jar))
        self.csrf = ""
        self.ids = {}
        self.results = []

    def normalize(self, value, key=""):
        if isinstance(value, dict):
            return {k: self.normalize(v, k) for k, v in value.items()}
        if isinstance(value, list):
            return [self.normalize(v, key) for v in value]
        if isinstance(value, str):
            if re.fullmatch(r"[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}", value):
                return self.ids.setdefault(value, f"$uuid-{len(self.ids)}")
            if key.endswith("_at") or key == "as_of":
                # Verify a timestamp is present without comparing wall-clock instants.
                from datetime import datetime
                stamp = re.sub(r"\.(\d+)(?=[+-])", lambda match: "." + match[1].ljust(6, "0")[:6], value.replace("Z", "+00:00"))
                stamp = re.sub(r"([+-]\d{2})$", r"\1:00", stamp)
                datetime.fromisoformat(stamp)
                return "$timestamp"
            for original,mapped in self.ids.items():
                value=value.replace(original,mapped)
        return value

    def request(self, path, body=None, expected=200, record=True):
        headers = {"User-Agent": "Monica synthetic differential"}
        if body is not None:
            headers.update({"Content-Type": "application/json", "X-CSRF-Token": self.csrf})
        req = urllib.request.Request(self.base + path, data=None if body is None else json.dumps(body).encode(), headers=headers)
        try:
            response = self.opener.open(req, timeout=20)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            payload = json.load(response)
            assert response.status == expected, (path, expected, response.status, payload)
            if record:
                self.results.append({"operation": req.get_method() + " " + re.sub(r"[0-9a-f-]{36}", "$uuid", path),
                                     "status": response.status, "body": self.normalize(payload)})
            if path in ("/api/v1/auth/csrf", "/api/v1/auth/login") and response.status == 200:
                cookie = response.headers.get("Set-Cookie", "").lower()
                assert "max-age=1209600" in cookie and "samesite=lax" in cookie, cookie
                assert ("httponly" in cookie) == path.endswith("/login"), cookie
            return payload


def code(secret):
    raw = base64.b32decode(secret + "=" * (-len(secret) % 8))
    digest = hmac.new(raw, struct.pack(">Q", int(time.time()) // 30), hashlib.sha1).digest()
    offset = digest[-1] & 15
    return f"{(int.from_bytes(digest[offset:offset+4], 'big') & 0x7fffffff) % 1000000:06}"


def scenario(base):
    client = Client(base)
    credentials = {"email": "fixture@example.invalid", "password": "synthetic-password-only"}
    client.request("/health")
    client.request("/api/v1/ready")
    client.request("/api/v1/me", expected=401)
    client.request("/api/v1/auth/bootstrap", credentials)
    client.request("/api/v1/auth/bootstrap", credentials, expected=409)
    client.csrf = client.request("/api/v1/auth/csrf", record=False)["csrf_token"]
    client.request("/api/v1/auth/login", dict(credentials, password="wrong-password-only"), expected=401)
    client.request("/api/v1/auth/login", credentials)
    client.request("/api/v1/me")
    client.request("/api/v1/auth/sessions")
    client.request("/api/v1/audit/events")
    client.request("/api/v1/auth/totp/status")
    setup = client.request("/api/v1/auth/totp/setup", {}, record=False)
    parsed = urllib.parse.urlsplit(setup["otpauth_url"])
    query = urllib.parse.parse_qs(parsed.query)
    client.results.append({"totp_scheme": parsed.scheme, "totp_type": parsed.netloc,
                           "issuer": query["issuer"], "secret_matches": query["secret"] == [setup["secret"]]})
    client.request("/api/v1/auth/totp/verify", {"code": "0000000"}, expected=422)
    verified = client.request("/api/v1/auth/totp/verify", {"code": code(setup["secret"])}, record=False)
    assert verified["status"] == "verified"
    assert len(verified["recovery_codes"]) == 8
    assert len(set(verified["recovery_codes"])) == 8
    assert all(re.fullmatch("[0-9a-f]{16}", c) for c in verified["recovery_codes"])
    client.request("/api/v1/auth/totp/status")
    client.request("/api/v1/auth/totp/setup", {}, expected=409)
    client.request("/api/v1/auth/logout", {})
    client.request("/api/v1/me", expected=401)
    client.request("/api/v1/auth/login", credentials, expected=401)
    recovery = dict(credentials, recovery_code=" " + verified["recovery_codes"][0] + " ")
    client.request("/api/v1/auth/login", recovery)
    client.request("/api/v1/auth/logout", {})
    client.request("/api/v1/auth/login", recovery, expected=401)
    client.request("/api/v1/auth/login", dict(credentials, totp_code=code(setup["secret"])))
    sessions = client.request("/api/v1/auth/sessions")
    current = next(s for s in sessions if s["current"])
    client.request(f'/api/v1/auth/sessions/{current["id"]}/revoke', {})
    client.request("/api/v1/me", expected=401)
    for _ in range(5):
        client.request("/api/v1/auth/login", dict(credentials, password="wrong-password-only"), expected=401)
    client.request("/api/v1/auth/login", dict(credentials, password="wrong-password-only"), expected=429)
    return client.results


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", choices=("crm", "desktop"), default="crm")
    args = parser.parse_args()
    with ReferenceBackend(args.baseline) as reference:
        expected = scenario(reference.url)
        subprocess.run([str(PG / "createdb"), "-h", str(reference.root), "rust"], check=True)
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        env = {"PATH": os.defpath, "MONICA_DATABASE_URL": f"postgresql:///rust?host={reference.root}",
               "MONICA_SESSION_SECRET": "synthetic-reference-session-secret-32bytes",
               "MONICA_MEDIA_ROOT": str(reference.root / "rust-media")}
        binary = str(Path("target/debug/monica-server").resolve())
        subprocess.run([binary, "migrate"], env=env, check=True)
        with (reference.root / "rust.log").open("wb") as log:
            rust = subprocess.Popen([binary, "serve", "--listen", f"127.0.0.1:{port}"], env=env, stdout=log, stderr=log)
            try:
                url = f"http://127.0.0.1:{port}"
                for _ in range(200):
                    if rust.poll() is not None:
                        raise RuntimeError((reference.root / "rust.log").read_text())
                    try:
                        urllib.request.urlopen(url + "/health", timeout=1).close()
                        break
                    except OSError:
                        time.sleep(.05)
                actual = scenario(url)
                assert actual == expected, json.dumps({"baseline": expected, "rust": actual}, ensure_ascii=False, indent=2)
                print(f"Real {args.baseline}/Rust auth lifecycle differential passed: {len(actual)} observations")
            finally:
                if rust.poll() is None:
                    rust.send_signal(signal.SIGTERM)
                    try:
                        rust.wait(timeout=100)
                    except subprocess.TimeoutExpired:
                        rust.kill()
                        rust.wait(timeout=5)


if __name__ == "__main__":
    main()
