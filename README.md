# LetWeMarket Content Agent Tool API

Internal, tool-based content-intelligence service (Rust). LetWeMarket's platform and AI Agent call it to ingest sources,
extract content, build reports, draft and repurpose social posts, source images, and hand **approved** content to Zernio.
It is *not* a social-media app: customer UI, brands, approvals UI, calendar, billing and quotas stay in LetWeMarket;
connected accounts, OAuth, publishing, inbox and analytics stay in Zernio. This service never stores customer OAuth tokens.

```
LetWeMarket ──HMAC-signed REST /internal/v1──▶ content-agent-api ──▶ Postgres (state, audit, durable queue)
                                                     │                       ▲
                                                     └── queue ──▶ content-agent-worker ──▶ HTTP / Obscura / FireCrawl /
                                                                                         GitHub / X / YouTube / LLM / images
                                                                  Zernio (publish, schedule, reads) ◀──┘
```

## What is implemented

| Area | Where |
|---|---|
| 10 required tools + `read_x_post`, `read_youtube_video`, `read_comments`, `read_inbox`, `read_performance`, `publish_draft`, `schedule_draft`, `get_job`, `get_draft`, `cancel_job` | [src/tools/](src/tools) — contracts in [docs/TOOLS.md](docs/TOOLS.md) |
| Versioned REST + OpenAPI 3.1 | [src/api.rs](src/api.rs), [docs/openapi.json](docs/openapi.json) (generated, checked by a test) |
| Service-to-service auth (HMAC-SHA256, rotation, replay window) | [src/auth.rs](src/auth.rs) |
| Request context, tenant/brand isolation, tool/channel/action permissions, quota + plan checks | [src/context.rs](src/context.rs), every store call is `(tenant, brand)`-scoped |
| Rate limits per tenant, brand, tool, provider | [src/ratelimit.rs](src/ratelimit.rs) |
| SSRF protection (scheme, DNS-resolved IP checks, pinned connections, redirect re-validation, size/time limits), robots.txt | [src/ssrf.rs](src/ssrf.rs), [src/urls.rs](src/urls.rs), [src/extract/robots.rs](src/extract/robots.rs) |
| `ContentExtractor` trait + Http, Obscura, FireCrawl, GitHub, X, YouTube extractors, with the spec's primary/fallback chains | [src/extract/](src/extract) |
| LLM provider abstraction (Anthropic, OpenAI-compatible, mock), retries/backoff, token limits, cost capture, prompt/model versions on every artifact | [src/llm/](src/llm), [src/generation.rs](src/generation.rs) |
| Image pipeline (candidates → format/size/aspect/safety validation → rank → object storage) | [src/images.rs](src/images.rs), [src/storage.rs](src/storage.rs) |
| Pre-publication validation (limits, hashtags, media, links, brand rules, prohibited content, duplicates, attribution, alt text) | [src/validate.rs](src/validate.rs) |
| Durable workflow state machine with persisted, audited transitions | [src/workflow.rs](src/workflow.rs) |
| Durable queue: leases, at-least-once, backoff + jitter, dead letters, cancellation, drain | [src/queue.rs](src/queue.rs), [src/worker.rs](src/worker.rs) |
| Zernio adapter: read/write separation, per-channel payloads, idempotent submit, error mapping, partial success | [src/zernio.rs](src/zernio.rs) |
| Postgres schema + migrations | [migrations/](migrations) |
| Metrics (Prometheus), JSON logs with trace/correlation ids, alerts | [src/observability.rs](src/observability.rs), [deploy/prometheus/alerts.yml](deploy/prometheus/alerts.yml) |
| Deployment | [Dockerfile](Dockerfile), [docker-compose.yml](docker-compose.yml), [deploy/k8s/](deploy/k8s) |

## Quick start

```bash
cp .env.example .env                       # mock LLM/images + local storage: no external credentials needed
docker compose up --build                  # postgres + api + worker
cargo test                                 # unit + integration + contract tests (in-memory store)
TEST_DATABASE_URL=postgres://postgres:postgres@localhost:5432/content_agent cargo test --test postgres
cargo test --release --test load -- --ignored --nocapture   # load / backpressure tests
```

## Developer console

Set `CONSOLE_ENABLED=true` (development/staging only — the service refuses to start with it in production) and open `/console`.
One static page, no framework and no build step, styled with the LWM design tokens from the Figma design system. It lists the
tools from `GET /internal/v1/tools`, signs each request in the browser (the secret you type stays in memory, never stored),
captures IDs from responses (`{{draft_id}}` placeholders chain the workflow steps), and keeps a call history with trace IDs.
Served with a strict CSP, `no-store`, and no inline data rendering (`textContent` only).

## Calling the API

Every request is signed. `signature = hex(HMAC_SHA256(secret, "{timestamp}\n{METHOD}\n{path_and_query}\n{sha256_hex(body)}"))`
with headers `x-lwm-timestamp` (unix seconds, ±5 min) and `x-lwm-signature`. Any secret in `INTERNAL_AUTH_SECRETS` verifies,
so rotate by deploying `new,old`, switching LetWeMarket to `new`, then dropping `old`.

```python
import hmac, hashlib, json, time, urllib.request
def call(base, secret, tool, context, input):
    path = f"/internal/v1/tools/{tool}"
    body = json.dumps({"context": context, "input": input}).encode()
    ts = str(int(time.time()))
    sig = hmac.new(secret.encode(), f"{ts}\nPOST\n{path}\n{hashlib.sha256(body).hexdigest()}".encode(), hashlib.sha256).hexdigest()
    req = urllib.request.Request(base + path, body, {"content-type": "application/json", "x-lwm-timestamp": ts, "x-lwm-signature": sig})
    return json.load(urllib.request.urlopen(req))
```

`context` is the validated request context from the spec (tenant, brand, actor, request id, idempotency key, permissions) plus,
when relevant, `zernio` (`zernio_profile_id` + `connected_accounts[{platform, account_id, access, tenant_id?, brand_id?}]`),
`plan_channels` and `quota_remaining`. **LetWeMarket owns those decisions; this service enforces them** and never widens them.

## Workflow

```
ingest_url ─▶ [worker] extract ─▶ generate_content_report ─▶ generate_social_draft / repurpose_content
  ─▶ find_images ─▶ validate_content ─▶ review_draft ─▶ (LetWeMarket UI) ─▶ submit_review_decision
  ─▶ prepare_publish_request ─▶ publish_draft | schedule_draft (inline or async via queue) ─▶ Zernio
```

Guarantees enforced in code and covered by tests: a draft can only reach Zernio from `approved` → `ready_for_zernio`; a read-only
connection can never publish; nothing is sent to Zernio when a check fails; retries reuse one Zernio `Idempotency-Key`;
tenant A can never read or mutate tenant B's (or another brand's) jobs, sources, reports, drafts or publish requests.

## Errors

```json
{ "error": { "code": "ZERNIO_CONNECTION_MISSING", "message": "...", "details": {}, "retryable": false,
             "documentation_url": "https://internal.docs.letwemarket.com/errors/ZERNIO_CONNECTION_MISSING" } }
```
All 17 required codes are implemented, plus `NOT_FOUND` and `INVALID_STATE`. Retryable errors set `retryable: true`.

## Documentation

[docs/TOOLS.md](docs/TOOLS.md) · [docs/RUNBOOK.md](docs/RUNBOOK.md) · [docs/SECURITY_CHECKLIST.md](docs/SECURITY_CHECKLIST.md) · [docs/openapi.json](docs/openapi.json) · [.env.example](.env.example)
