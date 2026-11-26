# Developer runbook

## Processes
* `content-agent-api` — HTTP only. `--print-openapi` prints the spec; `--migrate-only` applies migrations and exits (use as a deploy Job).
* `content-agent-worker` — leases tasks from Postgres (`queue_tasks`). Needs `DATABASE_URL`. SIGTERM stops *new* leases and lets in-flight tasks finish (set `terminationGracePeriodSeconds` ≥ `TOOL_TIMEOUT_SECS` + margin).
* Run the Obscura pool as a separate Deployment (`--target obscura-worker`) with `EXTRACTOR_OBSCURA_ENABLED=true`; keep it `false` on the general pool. Tasks are not pinned to a pool, so for strict separation run the general pool with Obscura disabled and the browser pool with `PREFER_FIRECRAWL=false`; a general-pool worker that cannot render escalates to FireCrawl or fails *retryably* and another worker picks it up.

## Environments
`APP_ENV=development|staging|production`. Staging/production refuse to boot with the mock LLM/image provider, a missing database, non-S3 storage, or `ALLOW_HTTP=true`. Never reuse secrets across environments.

## Common operations
| Task | How |
|---|---|
| Inspect a job/draft | tool `get_job` / `get_draft` (includes the audit trail with actor, reason, trace id) |
| Cancel a job | tool `cancel_job` (cancels queued tasks; an in-flight extraction finishes but is discarded) |
| See dead letters | `SELECT id, kind, tenant_id, job_id, attempts, last_error FROM queue_tasks WHERE state='dead' ORDER BY updated_at DESC;` |
| Replay a dead letter | `UPDATE queue_tasks SET state='pending', attempts=0, run_after=now(), last_error=NULL WHERE id='…';` (handlers are idempotent) |
| Roll the auth secret | deploy `INTERNAL_AUTH_SECRETS=new,old` → switch the caller → deploy `new` |
| Disable a tool/extractor | `DISABLED_TOOLS=publish_draft,schedule_draft` / `EXTRACTOR_*_ENABLED=false` (rolling restart) |
| Trace one request | grep logs for `trace_id` (returned in the `x-trace-id` header and stored on `tool_invocations` and `state_transitions`) |
| Retention | worker purges extracted text/screenshot refs older than `RETENTION_SOURCE_DAYS`; add an S3 lifecycle rule for screenshot/image objects; prompts and raw LLM output are **not stored** (only prompt/model versions, tokens, cost) |

## Failure modes
* **Zernio rate limited / outage** → `ZERNIO_RATE_LIMITED` / `ZERNIO_PUBLISH_FAILED (retryable)`; the draft returns to `ready_for_zernio` and the retry reuses the stored `Idempotency-Key`, so Zernio de-duplicates. Async submissions retry with backoff and dead-letter after 5 attempts.
* **Partial success** → publish request `partial_success`, draft stays `submitted_to_zernio`; per-channel results are in the response and stored `zernio_response`. Retry only the failed channels through Zernio (`POST /v1/posts/{id}/retry`) — do not re-submit the whole post.
* **Worker crash mid-task** → the lease expires and another worker re-runs it (at-least-once; handlers are idempotent).
* **Extraction keeps failing** → check `extraction_attempts_total{extractor,outcome}`; flip `PREFER_FIRECRAWL` or disable the failing extractor.
* **Queue backlog** → scale workers (`queue_depth`, `queue_oldest_pending_age_seconds` alerts).

## Known limits (be aware before relying on them)
* Rate limiting is per process. For strict global limits across replicas, back `RateLimiter` with Redis.
* Obscura is driven through its CLI (`obscura fetch`), one process per job; CPU/memory ceilings come from the container cgroup, not from the process.
* The YouTube Data API does not expose transcripts without owner OAuth; transcript-level understanding needs a Vertex AI/Gemini adapter or a Zernio-connected account (not built).
* X: single-post extraction via the official API; threads and the Zernio-connected-account path are not built.
* Zernio read endpoints (`/v1/inbox/comments`, `/v1/inbox/conversations`, `/v1/analytics`) follow Zernio's documented resource families — confirm exact paths/params for your plan. `POST /v1/posts` follows the published Posting API.
* Thread/carousel `parts` are passed to Zernio under `platformSpecificData.parts`; confirm Zernio's per-platform field for multi-part posts before enabling thread publishing.
* OpenTelemetry export is not wired; correlation/trace ids are propagated through logs, the database and response headers, and metrics are Prometheus-native.
