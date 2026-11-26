# Learning guide: what this project is and how to explore it

## Why it exists (in one minute)
LetWeMarket is moving to a **tool-based architecture**: every capability is a small, independent *tool* (like "create post" or
"get ads") with a fixed JSON contract. Today LetWeMarket's UI and AI Agent call those tools; in about six months any AI system
(Claude, ChatGPT) will connect through an MCP server and use the same tools, so nothing depends on the UI.

This service is the **content** set of tools. It is written in **Rust** on purpose: a JS version would drag in ~1.5 GB of
`node_modules` for every new internal API. Here each program is a ~10 MB binary using ~10–16 MB of memory (measured), and a
failing tool is easy to find, fix, and understand — for people and for AI coding tools.

It does **not** duplicate LetWeMarket (UI, brands, approvals, calendar, billing) or Zernio (social accounts, OAuth, publishing).

## Try it (about 10 minutes)
```bash
./scripts/dev.sh start     # Postgres + fake Zernio + API + worker (first build takes a few minutes)
./scripts/dev.sh demo      # guided walkthrough: explains each step and proves the safety rules
./scripts/dev.sh demo --pause   # same, but waits for Enter so you can read along
```
Then open **http://127.0.0.1:8787/console** (secret: `dev-secret-dev-secret-dev-secret-1234`) and run the tools yourself:
`ingest_url → extract_content → generate_content_report → generate_social_draft → find_images → validate_content →
review_draft → submit_review_decision → prepare_publish_request → schedule_draft`.
Use `./scripts/dev.sh logs worker|api|zernio` to watch what happens, and `./scripts/dev.sh test` to run all tests.

The demo ingests a Wikipedia article, so you need internet. Use `--url` for any other public article.
It uses a **mock** LLM and mock images (offline, no keys) — the pipeline is real, the wording is canned. For real text:
`LLM_PROVIDER=anthropic LLM_API_KEY=... ./scripts/dev.sh start`.

## Which URLs can it read?
Tested live on the dev stack (mock LLM, no API keys):

| URL kind | Works? | How / notes |
|---|---|---|
| Normal web pages: articles, blogs, docs, Wikipedia, company pages | Yes | Lightweight HTTP extractor. Follows HTTP redirects and tiny "redirect stub" pages (meta-refresh / JS redirect). |
| GitHub repo, file, issue, pull request | Yes (public) | GitHub API. Private repos and higher rate limits need `GITHUB_TOKEN`. |
| Pages that only render with JavaScript (apps, some marketing sites) | Only with a renderer | Needs the Obscura binary (`OBSCURA_BIN`) or a `FIRECRAWL_API_KEY`. Otherwise the error says exactly that. |
| X (Twitter) post | Only with `X_BEARER_TOKEN` | Official X API; single posts, not threads. |
| YouTube video | Only with `YOUTUBE_API_KEY` | Title, description, tags, thumbnail. No transcript. |
| Instagram / Facebook / LinkedIn pages | No | Their robots.txt forbids bots, so the service refuses (by design). They are *output channels*, not sources. |
| PDFs, images, other files | No | Only HTML/text pages. |
| Pages behind a login or paywall | No | 401/403 is never bypassed. |
| `http://`, `localhost`, private/internal IPs, odd ports, URLs with passwords | Rejected | SSRF protection. |

Limits: 20 URLs per `ingest_url` call; 5 MB per page; 15 s timeout; 5 redirects; 120,000 characters of text kept.
A URL already ingested for the same brand is rejected as `DUPLICATE_SOURCE` unless you pass `"allow_duplicates": true`.
Ingest only *checks and queues* the URL; whether a page is readable is decided later by the worker (see `get_job`).

## The picture
```
LetWeMarket ──signed request──▶ API ──▶ Postgres (state + audit + queue)
                                 │                 ▲
                                 └─ enqueue ─▶ Worker ──▶ web / Obscura / FireCrawl / GitHub / X / YouTube / LLM
                         approved drafts only ─▶ Zernio (fake in dev) ─▶ social networks
```

## Ideas worth understanding (each is tested)
| Idea | Plain meaning | Where / test |
|---|---|---|
| **Tool** | One small capability with a stable input/output JSON | `src/tools/`, `docs/TOOLS.md` |
| **HMAC signing** | Proves a request came from LetWeMarket and wasn't changed | `src/auth.rs`, `tests/security.rs` |
| **Tenant/brand isolation** | One customer can never see another's data; every query is scoped | `src/store/`, `tenant_and_brand_isolation` |
| **SSRF guard** | Don't let users make the server fetch internal URLs | `src/ssrf.rs`, `src/urls.rs` |
| **Async job + queue** | Slow work runs in a worker; the API answers immediately | `src/queue.rs`, `src/worker.rs` |
| **Idempotency** | Doing the same request twice has the same effect as once (no double posts) | `ingest_url`, `prepare_publish_request`, Zernio key |
| **State machine** | A draft can only move along allowed steps; approval can't be skipped | `src/workflow.rs` |
| **Audit trail** | Every state change records who/why/when/trace id | `get_draft` → `events` |
| **Extractor chain** | Cheap HTTP first → browser (Obscura) → managed (FireCrawl) | `src/extract/mod.rs` |
| **Read vs write access** | Reading accounts never allows publishing | `src/zernio.rs` |

## Suggested reading order
1. `docs/TOOLS.md` — what each tool takes and returns.
2. `src/tools/mod.rs` — how every call is authorised, rate-limited, timed and recorded.
3. `src/workflow.rs` — the lifecycle (tiny, with tests).
4. `src/tools/ingest.rs` → `src/pipeline.rs` → `src/extract/mod.rs` — a request becoming a job and extracted content.
5. `tests/e2e_flow.rs` — the whole flow as one readable test.

## Experiments to build intuition
* Run `demo`, then stop the worker (`kill $(cat .dev/worker.pid)`), ingest in the console, watch the job stay `queued`; restart with
  `./scripts/dev.sh start` and watch it finish — the queue is durable.
* In the console, call `schedule_draft` twice with the same `publish_request_id`; check `./scripts/dev.sh logs zernio` — only one post.
* Edit the context JSON in the console: remove `"schedule"` from `allowed_actions` and see `FORBIDDEN`; set a connected account's
  `access` to `"read"` and see `ZERNIO_PERMISSION_DENIED`.
* Open Postgres: `docker exec -it content-agent-dev-pg psql -U postgres content_agent` and `select * from state_transitions;`.
