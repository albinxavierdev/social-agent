#!/usr/bin/env python3
"""Guided walkthrough of the Content Agent. Run it with:  ./scripts/dev.sh demo   (after ./scripts/dev.sh start)

It plays the role of LetWeMarket: it signs requests, calls the tools in order, and after each step explains
WHAT the tool did and WHY it exists. It also tries things that must be refused, to show the safety rules working.
Standard library only.

Options:  --url URL    article to ingest (default: a Wikipedia article about Rust; needs internet)
          --pause      wait for Enter between steps (good for reading along)
"""
import argparse, hashlib, hmac, json, os, sys, time, urllib.request, urllib.error, uuid
from datetime import datetime, timedelta, timezone

BASE = os.environ.get("API_BASE", "http://127.0.0.1:8787")
SECRET = os.environ.get("DEV_SECRET", "dev-secret-dev-secret-dev-secret-1234")
TTY = sys.stdout.isatty()
B, D, G, Y, R, P, N = (("\033[1m", "\033[2m", "\033[32m", "\033[33m", "\033[31m", "\033[35m", "\033[0m") if TTY else ("",) * 7)
results = []  # (label, passed)


def sign_and_send(method, path, body=b""):
    ts = str(int(time.time()))
    sig = hmac.new(SECRET.encode(), f"{ts}\n{method}\n{path}\n{hashlib.sha256(body).hexdigest()}".encode(), hashlib.sha256).hexdigest()
    req = urllib.request.Request(BASE + path, data=body or None, method=method,
                                 headers={"content-type": "application/json", "x-lwm-timestamp": ts, "x-lwm-signature": sig})
    return _do(req)


def _do(req):
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            return r.status, json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e:
        raw = e.read()
        try:
            return e.code, json.loads(raw)
        except Exception:
            return e.code, {"raw": raw.decode(errors="replace")}


def context(tenant, brand, access="write", actions=("read", "draft", "publish", "schedule")):
    chans = ["linkedin", "x"]
    return {
        "tenant_id": tenant, "brand_id": brand, "actor_id": "user_demo", "request_id": "req_" + uuid.uuid4().hex, "source": "letwemarket_agent",
        "permissions": {"allowed_tools": ["*"], "allowed_channels": chans, "allowed_actions": list(actions)},
        "zernio": {"zernio_profile_id": "profile_demo", "connected_accounts": [{"platform": c, "account_id": f"acc_{c}", "access": access} for c in chans]},
        "quota_remaining": 1000,
    }


def call(tool, ctx, inp):
    body = json.dumps({"context": ctx, "input": inp}).encode()
    return sign_and_send("POST", f"/internal/v1/tools/{tool}", body)


def short(v, n=150):
    s = json.dumps(v, ensure_ascii=False)
    return s if len(s) <= n else s[: n - 1] + "…"


def title(n, text):
    print(f"\n{P}━━ Step {n}: {text}{N}")


def why(text):
    print(f"   {D}WHY:{N} {text}")


def what(text):
    print(f"   {B}{text}{N}")


def check(label, cond, detail=""):
    results.append((label, bool(cond)))
    print(f"   {G + '✓' if cond else R + '✗'}{N} {label}" + (f"  {D}{detail}{N}" if detail else ""))
    return cond


def expect_ok(tool, ctx, inp):
    s, v = call(tool, ctx, inp)
    if s != 200:
        print(f"   {R}✗ {tool} failed: HTTP {s} {short(v, 400)}{N}")
        results.append((f"{tool} succeeds", False))
        finish()
    return v


def expect_err(label, tool, ctx, inp, code):
    s, v = call(tool, ctx, inp)
    got = (v or {}).get("error", {}).get("code") if isinstance(v, dict) else None
    check(label, s != 200 and got == code, f"→ HTTP {s} {got}" + (f" — {v['error']['message']}" if got else ""))
    return v


def finish():
    print(f"\n{P}━━ Summary{N}")
    bad = [l for l, ok in results if not ok]
    print(f"   {len(results) - len(bad)}/{len(results)} checks passed")
    for l in bad:
        print(f"   {R}✗ {l}{N}")
    sys.exit(1 if bad else 0)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", default="https://en.wikipedia.org/wiki/Rust_(programming_language)")
    ap.add_argument("--pause", action="store_true")
    a = ap.parse_args()
    pause = (lambda: input(f"   {D}[Enter to continue]{N} ")) if a.pause else (lambda: None)

    run = uuid.uuid4().hex[:6]
    tenant, brand = f"tenant_demo_{run}", f"brand_demo_{run}"
    ctx = context(tenant, brand)
    print(f"{B}Content Agent walkthrough{N}  (tenant={tenant}, brand={brand})")
    print(f"{D}You are playing LetWeMarket, the platform that owns users, brands, approvals and billing.\n"
          f"The Content Agent only does the content work — and refuses anything LetWeMarket didn't authorise.{N}")

    # 0 ------------------------------------------------------------------------------------------------------
    title(0, "Is the service alive, and what tools does it offer?")
    with urllib.request.urlopen(BASE + "/readyz") as r:
        ready = json.loads(r.read())
    check("readiness: database + queue answer", ready.get("status") == "ready", short(ready))
    s, tools = sign_and_send("GET", "/internal/v1/tools")
    names = [t["name"] for t in tools["tools"]]
    check(f"tool registry lists {len(names)} tools", s == 200 and len(names) >= 20)
    what("Tools are independent 'lego pieces': " + ", ".join(names[:6]) + ", …")
    why("Every capability is one small tool with a fixed JSON contract. That's what lets LetWeMarket's UI, an AI agent, or later an MCP client\n"
        "        (Claude, ChatGPT) all use the same pieces — and lets you find exactly which tool is misbehaving.")
    pause()

    # 1 ------------------------------------------------------------------------------------------------------
    title(1, "Authentication: only LetWeMarket may call this")
    body = json.dumps({"context": ctx, "input": {"job_id": "x"}}).encode()
    req = urllib.request.Request(BASE + "/internal/v1/tools/get_job", data=body, method="POST", headers={"content-type": "application/json"})
    s, v = _do(req)
    check("unsigned request is rejected", s == 401 and v["error"]["code"] == "UNAUTHORIZED", f"→ {s} {v['error']['code']}")
    why("Requests are signed with HMAC-SHA256 over the method, path, body and a timestamp. Without the shared secret nobody can call a tool,\n"
        "        and a captured request can't be replayed after 5 minutes or edited (the signature would no longer match).")
    pause()

    # 2 ------------------------------------------------------------------------------------------------------
    title(2, "SSRF protection: the service refuses to fetch internal addresses")
    for bad in ("https://169.254.169.254/latest/meta-data/", "https://localhost/admin", "http://example.com/plain-http"):
        expect_err(f"blocked: {bad}", "ingest_url", ctx, {"urls": [bad]}, "URL_NOT_ALLOWED")
    why("This service fetches URLs people give it. Without this guard someone could point it at cloud metadata or internal admin pages\n"
        "        (SSRF). It blocks private/loopback/link-local ranges, re-checks every redirect, and pins DNS so it can't be tricked mid-request.")
    pause()

    # 3 ------------------------------------------------------------------------------------------------------
    title(3, f"ingest_url — queue a job for {a.url}")
    ing = expect_ok("ingest_url", ctx, {"urls": [a.url], "requested_channels": ["linkedin", "x"],
                                         "brand_context": {"tone_of_voice": "friendly and clear", "content_rules": ["no jargon"]}})
    job_id, src = ing["job_id"], ing["sources"][0]
    what(f"job {job_id}  status={ing['status']}  type={src['source_type']}  strategy={src['extraction_strategy']}")
    check("job is queued (not processed inline)", ing["status"] == "queued")
    why("Long work is asynchronous. The API validates the URL, records a job, puts a task on a durable queue (in Postgres) and answers immediately.\n"
        "        A separate worker process picks it up. If the worker crashes, the task is re-leased — nothing is lost.")
    pause()

    # 4 ------------------------------------------------------------------------------------------------------
    title(4, "The worker extracts the page (we poll get_job)")
    deadline, status = time.time() + 90, None
    while time.time() < deadline:
        j = expect_ok("get_job", ctx, {"job_id": job_id})
        status = j["sources"][0]["extraction_status"]
        if status != "pending":
            break
        time.sleep(1)
    if status != "success":
        print(f"   {R}Extraction ended as '{status}': {short(j['sources'][0].get('error'))}{N}")
        print("   Is this machine online? Wikipedia must be reachable. Try --url with another public article, or see: ./scripts/dev.sh logs worker")
        results.append(("extraction succeeded", False))
        finish()
    ex = expect_ok("extract_content", ctx, {"source_id": src["source_id"]})
    check("extracted with the lightweight HTTP extractor first", ex["extraction_method"] == "http", f"method={ex['extraction_method']}")
    what(f"title: {ex['title']!r}  language={ex['language']}  {len(ex['text'])} characters of clean text")
    why("Static pages use the cheap HTTP extractor. Only pages that need JavaScript escalate to the Obscura browser, and only if that fails does it\n"
        "        fall back to FireCrawl. It respects robots.txt and never tries to bypass logins or paywalls.")
    pause()

    # 5 ------------------------------------------------------------------------------------------------------
    title(5, "generate_content_report — turn the source into a plan")
    rep = expect_ok("generate_content_report", ctx, {"source_ids": [src["source_id"]], "campaign_goal": "awareness", "requested_channels": ["linkedin", "x"]})
    what(f"report {rep['report_id']}  angle: {rep['recommended_angle'][:90]!r}")
    print(f"   key points: {short(rep['key_points'][:2], 200)}")
    check("report cites its source links", bool(rep["source_links"]))
    why("Two-stage generation (like the reference project): source → structured report → posts. The report records key points, risks and sources,\n"
        "        so every post is traceable to what it was based on. The model + prompt version are stored with it (this run uses the offline MOCK model).")
    pause()

    # 6 ------------------------------------------------------------------------------------------------------
    title(6, "generate_social_draft — one draft, a variant per channel")
    d = expect_ok("generate_social_draft", ctx, {"report_id": rep["report_id"], "channels": ["linkedin", "x"], "post_type": "text"})
    draft_id = d["draft_id"]
    for v in d["variants"]:
        print(f"   {B}{v['channel']}{N}: {v['caption'][:110]!r}  {' '.join(v['hashtags'])}")
    check("draft is NOT published, only draft_ready", d["status"] == "draft_ready")
    why("Each channel has its own limits (X: 280 chars with links counting as 23, hashtag caps, media rules). Nothing is published from this step —\n"
        "        publishing is a separate, approval-gated path.")
    pause()

    # 7 ------------------------------------------------------------------------------------------------------
    title(7, "find_images + validate_content — make it publishable")
    img = expect_ok("find_images", ctx, {"draft_id": draft_id, "count": 1})
    im = img["images"][0]
    check("image validated (format, size, aspect ratio, safety)", im["validation_status"] == "passed", f"{im['width']}x{im['height']} {im['aspect_ratio']}")
    val = expect_ok("validate_content", ctx, {"draft_id": draft_id})
    check("pre-publication checks pass", val["status"] == "passed", f"warnings={short(val['warnings'], 120)}")
    why("Images are checked by their real bytes (not the declared type) and stored under tenant/brand paths. Validation covers limits, hashtags, links,\n"
        "        prohibited phrases, duplicates, attribution and alt text — so problems are caught before a human or a social network sees them.")
    pause()

    # 8 ------------------------------------------------------------------------------------------------------
    title(8, "review_draft — hand over to a human (the UI lives in LetWeMarket)")
    rv = expect_ok("review_draft", ctx, {"draft_id": draft_id})
    what(f"status={rv['status']}  next_action={rv['next_action']}  review_url={rv['review_url']}")
    why("This service only tracks the review STATE. The review screen, calendar and approvals stay in LetWeMarket — no duplicated product features.")
    pause()

    # 9 ------------------------------------------------------------------------------------------------------
    title(9, "Safety rails — things that MUST be refused")
    when = (datetime.now(timezone.utc) + timedelta(days=2)).strftime("%Y-%m-%dT%H:%M:%SZ")
    prep_in = {"draft_id": draft_id, "zernio_profile_id": "profile_demo", "channels": ["linkedin", "x"], "action": "schedule", "scheduled_at": when}
    expect_err("cannot publish an unapproved draft", "prepare_publish_request", ctx, prep_in, "INVALID_STATE")
    other = context(f"tenant_other_{run}", brand)
    expect_err("another tenant cannot read this draft", "get_draft", other, {"draft_id": draft_id}, "NOT_FOUND")
    other_brand = context(tenant, f"brand_other_{run}")
    expect_err("another brand in the same tenant cannot read it either", "get_draft", other_brand, {"draft_id": draft_id}, "NOT_FOUND")
    expect_err("a caller without the 'draft' action cannot generate", "generate_social_draft",
               context(tenant, brand, actions=("read",)), {"report_id": rep["report_id"], "channels": ["x"]}, "FORBIDDEN")
    why("Approval is a hard gate; tenants and brands are isolated on every query; and each call carries the exact permissions LetWeMarket granted —\n"
        "        the service enforces them and never widens them.")
    pause()

    # 10 -----------------------------------------------------------------------------------------------------
    title(10, "submit_review_decision — the customer approves")
    dec = expect_ok("submit_review_decision", ctx, {"draft_id": draft_id, "decision": "approved", "reviewer_id": "user_reviewer", "notes": "Looks good"})
    check("draft is approved", dec["status"] == "approved")
    ro = context(tenant, brand, access="read")
    expect_err("a READ-ONLY Zernio connection cannot prepare a publish", "prepare_publish_request", ro, prep_in, "ZERNIO_PERMISSION_DENIED")
    why("Read and write access to connected social accounts are separate. Having a read connection never lets anything publish.")
    pause()

    # 11 -----------------------------------------------------------------------------------------------------
    title(11, "prepare_publish_request → schedule_draft → (fake) Zernio")
    prep = expect_ok("prepare_publish_request", ctx, prep_in)
    check("request is ready for Zernio", prep["status"] == "ready_for_zernio")
    again = expect_ok("prepare_publish_request", ctx, prep_in)
    check("preparing twice is idempotent (same request id)", again["publish_request_id"] == prep["publish_request_id"])
    sub = expect_ok("schedule_draft", ctx, {"publish_request_id": prep["publish_request_id"]})
    check("Zernio accepted every channel", sub["status"] == "scheduled_or_published", f"platforms={short(sub['platforms'], 140)}")
    replay = expect_ok("schedule_draft", ctx, {"publish_request_id": prep["publish_request_id"]})
    check("submitting again does NOT double-post", replay.get("replayed") is True)
    why("Zernio is the only thing that touches social accounts; this service holds just IDs, never OAuth tokens. Each submission carries one stable\n"
        "        Idempotency-Key, so a retry after a timeout can't create a duplicate post. See the fake Zernio's side: ./scripts/dev.sh logs zernio")
    pause()

    # 12 -----------------------------------------------------------------------------------------------------
    title(12, "The audit trail — every state change is recorded")
    g = expect_ok("get_draft", ctx, {"draft_id": draft_id})
    for e in g["events"]:
        print(f"   {e['from']:>24} → {e['to']:<24} by {e['actor_id']:<14} {D}{e['reason'][:55]}  [{e['trace_id'][:14]}…]{N}")
    states = [e["to"] for e in g["events"]]
    check("full lifecycle recorded", states == ["draft_ready", "awaiting_customer_review", "approved", "ready_for_zernio", "submitted_to_zernio", "scheduled_or_published"])
    why("Who did what, when, why, and under which trace id — queryable per draft or job. Metrics live at /metrics; logs carry the same trace ids.")

    print(f"\n{B}Next:{N} open the developer console at {BASE}/console (secret: {SECRET}) and repeat these steps by hand, or read docs/LEARN.md.")
    finish()


if __name__ == "__main__":
    main()
