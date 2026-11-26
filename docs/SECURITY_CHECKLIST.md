# Security review checklist

Status: ✅ implemented and tested · 🟡 implemented, needs deployment-side configuration · ⬜ not implemented

| Control | Status | Evidence |
|---|---|---|
| Service-to-service auth, requests without a valid identity rejected | ✅ | HMAC-SHA256 over method+path+body+timestamp, ±5 min window, rotation; `tests/security.rs::rejects_missing_bad_stale_and_tampered_signatures` |
| Tenant and brand isolation on every query and job | ✅ | all `Store` methods take a `Scope`; `tenant_and_brand_isolation`, `concurrent_multi_tenant_ingestion_stays_isolated`, same checks on Postgres |
| Tool-, channel-, action-level authorization | ✅ | `enforces_tool_action_channel_and_quota_permissions` |
| Separate read vs write; a read connection never publishes | ✅ | `read_connection_can_never_publish_…`, `zernio_read_tools_use_read_access_and_never_write` |
| Drafts cannot be published without explicit approval | ✅ | state machine + `prepare_publish_request` gate; `full_flow…`, `workflow::tests::cannot_skip_review` |
| No customer OAuth tokens held | ✅ | context carries ids only; only the service Zernio API key exists; test asserts no credentials in Zernio payloads |
| SSRF protection | ✅ | scheme/credentials/port checks, DNS-resolved IP deny-ranges (private, loopback, link-local/metadata, CGNAT, ULA, mapped/NAT64), connection pinned to the validated IP, redirects re-validated, body/time limits; `ssrf::tests`, `urls::tests`, `blocks_ssrf_targets_at_ingest` |
| Obscura contained | ✅/🟡 | fresh profile per job (wiped), pool cap, wall-clock + output caps, no `--allow-private-network`; cgroup CPU/memory limits and the egress `NetworkPolicy` are in `deploy/k8s` |
| Robots / no auth or paywall bypass | ✅ | `robots.rs`; 401/403 never retried with another method |
| URL allow/deny lists | ✅ | `URL_ALLOWLIST`, `URL_DENYLIST` |
| Rate limiting per tenant, brand, tool, provider | ✅ (per instance) | `ratelimit.rs`; Redis for global limits is ⬜ |
| Request size limits | ✅ | `MAX_BODY_BYTES` (layer + auth middleware) |
| Prompt-injection hardening | ✅ | scraped text only in a JSON data field, system prompt marks it untrusted, output parsed to typed structs, channels/links decided by code |
| Unsafe-file scanning for downloaded assets | 🟡 | magic-byte sniffing, size/dimension limits, active-content/polyglot detection; plug ClamAV behind `basic_scan` for malware signatures |
| Secrets in a secret manager, none in source/images | 🟡 | env-only config, `.env*` ignored, `.dockerignore`, `secret.example.yaml` has no values; wire your manager |
| Encryption in transit / at rest | 🟡 | TLS via rustls for all outbound calls; terminate inbound TLS/mTLS at the mesh/ingress; enable Postgres + S3 encryption at rest |
| Audit logs for sensitive actions | ✅ | `state_transitions`, `review_events`, `publish_requests`, `tool_invocations` (actor, trace id, input hash — never input bodies) |
| PII redaction in logs/traces | ✅ | bodies are never logged; secrets/emails scrubbed from provider errors (`llm::redact`); Zernio inbox data is returned, not logged |
| Retention policies | 🟡 | `RETENTION_SOURCE_DAYS` purge job; prompts/outputs not stored; add S3 lifecycle rules |
| Least-privilege containers | ✅ | non-root, read-only rootfs, dropped caps, `seccomp` (k8s manifests) |
| Dependency hygiene | 🟡 | run `cargo audit` / `cargo deny` in CI |
| mTLS / network policy between LetWeMarket and the API | ⬜ | HMAC is app-level; add mesh mTLS if required |
