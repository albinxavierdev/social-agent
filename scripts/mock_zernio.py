#!/usr/bin/env python3
"""A tiny fake Zernio for local learning. It stands in for the real Zernio API so you can watch the full
"approve -> prepare -> schedule" flow without any account, key, or real social post.

Real Zernio: one POST /v1/posts call publishes/schedules to many channels. This fake accepts the same shape,
remembers every Idempotency-Key it has seen (replays return the first answer, like the real one), and prints
each request so you can see exactly what this service sends -- and what it never sends (no tokens).
"""
import json, sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

SEEN = {}  # Idempotency-Key -> response body


class H(BaseHTTPRequestHandler):
    def log_message(self, *a):  # quiet default logging
        pass

    def _send(self, code, body):
        raw = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def do_POST(self):
        n = int(self.headers.get("content-length", 0))
        body = json.loads(self.rfile.read(n) or b"{}")
        if self.path != "/v1/posts":
            return self._send(404, {"message": "not found"})
        key = self.headers.get("Idempotency-Key", "")
        auth = "present" if self.headers.get("Authorization", "").startswith("Bearer ") else "MISSING"
        if key and key in SEEN:
            print(f"[mock-zernio] REPLAY  key={key[:18]}... -> returning the original post (no duplicate)", flush=True)
            return self._send(200, SEEN[key])
        status = "published" if body.get("publishNow") else "scheduled"
        post = {"post": {"_id": "zp_" + (key[-8:] or "demo"), "platforms": [
            {"platform": p["platform"], "status": status, "platformPostUrl": f"https://example.invalid/{p['platform']}/post"}
            for p in body.get("platforms", [])]}}
        SEEN[key] = post
        chans = ", ".join(f"{p['platform']}->{p['accountId']}" for p in body.get("platforms", []))
        print(f"[mock-zernio] POST /v1/posts  auth={auth}  idempotency-key={key[:18]}...  {status} to: {chans}", flush=True)
        for p in body.get("platforms", []):
            print(f"[mock-zernio]   {p['platform']}: {p.get('customContent', '')[:90]!r}", flush=True)
        self._send(200, post)

    def do_GET(self):
        if self.path.startswith("/v1/inbox/comments"):
            return self._send(200, {"comments": [{"id": "c1", "text": "Love this!", "platform": "instagram"}]})
        if self.path.startswith("/v1/inbox/conversations"):
            return self._send(200, {"conversations": []})
        if self.path.startswith("/v1/analytics"):
            return self._send(200, {"reach": 1234, "engagement": 56})
        self._send(404, {"message": "not found"})


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 18091
    print(f"[mock-zernio] listening on 127.0.0.1:{port}", flush=True)
    ThreadingHTTPServer(("127.0.0.1", port), H).serve_forever()
