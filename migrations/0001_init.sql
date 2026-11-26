-- Content Agent schema (spec §12). Each table keeps the spec's queryable columns
-- as typed columns and the full entity as `data jsonb`, so the Rust structs stay
-- the single source of truth while tenant/brand isolation and lookups stay indexed.

CREATE TABLE ingestion_jobs (
    id               text PRIMARY KEY,
    tenant_id        text NOT NULL,
    brand_id         text NOT NULL,
    actor_id         text NOT NULL,
    status           text NOT NULL,
    source_type      text,
    requested_channels text[] NOT NULL DEFAULT '{}',
    idempotency_key  text,
    created_at       timestamptz NOT NULL,
    updated_at       timestamptz NOT NULL,
    completed_at     timestamptz,
    error            jsonb,
    data             jsonb NOT NULL
);
CREATE UNIQUE INDEX ingestion_jobs_idem ON ingestion_jobs (tenant_id, brand_id, idempotency_key)
    WHERE idempotency_key IS NOT NULL;
CREATE INDEX ingestion_jobs_scope ON ingestion_jobs (tenant_id, brand_id, created_at DESC);

CREATE TABLE sources (
    id                 text PRIMARY KEY,
    job_id             text NOT NULL REFERENCES ingestion_jobs(id),
    tenant_id          text NOT NULL,
    brand_id           text NOT NULL,
    url                text NOT NULL,
    source_type        text NOT NULL,
    extraction_method  text,
    extraction_status  text NOT NULL,
    title              text,
    author             text,
    published_at       timestamptz,
    language           text,
    raw_content_url    text,
    normalized_content text,
    metadata           jsonb NOT NULL DEFAULT '{}',
    screenshot_url     text,
    content_hash       text,
    created_at         timestamptz NOT NULL,
    data               jsonb NOT NULL
);
CREATE INDEX sources_scope_job ON sources (tenant_id, brand_id, job_id);
CREATE INDEX sources_scope_url ON sources (tenant_id, brand_id, url);
CREATE INDEX sources_scope_hash ON sources (tenant_id, brand_id, content_hash) WHERE content_hash IS NOT NULL;

CREATE TABLE content_reports (
    id             text PRIMARY KEY,
    job_id         text NOT NULL REFERENCES ingestion_jobs(id),
    tenant_id      text NOT NULL,
    brand_id       text NOT NULL,
    source_ids     text[] NOT NULL,
    report_payload jsonb NOT NULL,
    model_provider text NOT NULL,
    model_version  text NOT NULL,
    prompt_version text NOT NULL,
    created_at     timestamptz NOT NULL,
    data           jsonb NOT NULL
);
CREATE INDEX content_reports_scope ON content_reports (tenant_id, brand_id);

CREATE TABLE content_drafts (
    id                text PRIMARY KEY,
    job_id            text NOT NULL REFERENCES ingestion_jobs(id),
    report_id         text NOT NULL REFERENCES content_reports(id),
    tenant_id         text NOT NULL,
    brand_id          text NOT NULL,
    channel           text NOT NULL,
    format            text NOT NULL,
    status            text NOT NULL,
    content_payload   jsonb NOT NULL,
    media_assets      jsonb NOT NULL DEFAULT '[]',
    source_links      jsonb NOT NULL DEFAULT '[]',
    compliance_status text NOT NULL,
    created_at        timestamptz NOT NULL,
    updated_at        timestamptz NOT NULL,
    data              jsonb NOT NULL
);
CREATE INDEX content_drafts_scope ON content_drafts (tenant_id, brand_id, created_at DESC);

CREATE TABLE review_events (
    id             text PRIMARY KEY,
    draft_id       text NOT NULL REFERENCES content_drafts(id),
    tenant_id      text NOT NULL,
    brand_id       text NOT NULL,
    reviewer_id    text NOT NULL,
    decision       text NOT NULL,
    edited_content jsonb,
    notes          text,
    created_at     timestamptz NOT NULL,
    data           jsonb NOT NULL
);
CREATE INDEX review_events_scope ON review_events (tenant_id, brand_id, draft_id);

CREATE TABLE publish_requests (
    id                     text PRIMARY KEY,
    draft_id               text NOT NULL REFERENCES content_drafts(id),
    tenant_id              text NOT NULL,
    brand_id               text NOT NULL,
    zernio_profile_id      text NOT NULL,
    channels               text[] NOT NULL,
    action                 text NOT NULL,
    scheduled_at           timestamptz,
    zernio_request_payload jsonb NOT NULL,
    zernio_response        jsonb,
    status                 text NOT NULL,
    created_at             timestamptz NOT NULL,
    updated_at             timestamptz NOT NULL,
    data                   jsonb NOT NULL
);
CREATE INDEX publish_requests_scope ON publish_requests (tenant_id, brand_id, draft_id);

CREATE TABLE tool_invocations (
    id          text PRIMARY KEY,
    tenant_id   text NOT NULL,
    brand_id    text NOT NULL,
    actor_id    text NOT NULL,
    tool_name   text NOT NULL,
    input_hash  text NOT NULL,
    status      text NOT NULL,
    latency_ms  bigint NOT NULL,
    token_usage jsonb,
    cost        double precision,
    error       jsonb,
    trace_id    text NOT NULL,
    created_at  timestamptz NOT NULL
);
CREATE INDEX tool_invocations_scope ON tool_invocations (tenant_id, brand_id, created_at DESC);

-- Persisted workflow transitions (spec §8).
CREATE TABLE state_transitions (
    id             text PRIMARY KEY,
    tenant_id      text NOT NULL,
    brand_id       text NOT NULL,
    entity_kind    text NOT NULL,
    entity_id      text NOT NULL,
    actor_id       text NOT NULL,
    from_state     text NOT NULL,
    to_state       text NOT NULL,
    reason         text NOT NULL,
    correlation_id text NOT NULL,
    trace_id       text NOT NULL,
    at             timestamptz NOT NULL
);
CREATE INDEX state_transitions_entity ON state_transitions (tenant_id, brand_id, entity_id, at);

-- Durable work queue (spec §9): at-least-once via SKIP LOCKED leasing,
-- exponential backoff with jitter, dead-letter state, cancellation.
CREATE TABLE queue_tasks (
    id            text PRIMARY KEY,
    kind          text NOT NULL,
    tenant_id     text NOT NULL,
    brand_id      text NOT NULL,
    job_id        text,
    payload       jsonb NOT NULL,
    state         text NOT NULL DEFAULT 'pending', -- pending | leased | done | dead | cancelled
    attempts      integer NOT NULL DEFAULT 0,
    max_attempts  integer NOT NULL DEFAULT 5,
    run_after     timestamptz NOT NULL DEFAULT now(),
    lease_until   timestamptz,
    last_error    text,
    correlation_id text NOT NULL,
    trace_id      text NOT NULL,
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX queue_tasks_ready ON queue_tasks (state, run_after);
CREATE INDEX queue_tasks_job ON queue_tasks (tenant_id, brand_id, job_id);
