//! PostgreSQL store (sqlx, runtime-checked queries so the crate builds without a DB).
//! Typed columns carry the indexed/queryable fields from spec §12; `data` carries the entity.

use super::Store;
use crate::context::Scope;
use crate::error::{ApiError, ApiResult};
use crate::model::*;
use crate::workflow::{Transition, WorkflowState};
use async_trait::async_trait;
use serde::{de::DeserializeOwned, Serialize};
use sqlx::{postgres::PgPoolOptions, PgPool, Row};

#[derive(Clone)]
pub struct PgStore {
    pub pool: PgPool,
}

fn to_json<T: Serialize>(v: &T) -> ApiResult<serde_json::Value> {
    serde_json::to_value(v).map_err(ApiError::internal)
}
fn from_json<T: DeserializeOwned>(v: serde_json::Value) -> ApiResult<T> {
    serde_json::from_value(v).map_err(ApiError::internal)
}

impl PgStore {
    pub async fn connect(url: &str, max_connections: u32) -> anyhow::Result<Self> {
        let pool = PgPoolOptions::new().max_connections(max_connections).acquire_timeout(std::time::Duration::from_secs(5)).connect(url).await?;
        Ok(Self { pool })
    }

    pub async fn migrate(&self) -> anyhow::Result<()> {
        sqlx::migrate!("./migrations").run(&self.pool).await?;
        Ok(())
    }

    async fn get_one<T: DeserializeOwned>(&self, table: &str, s: &Scope, id: &str) -> ApiResult<Option<T>> {
        // `table` is always a compile-time literal from this module, never user input.
        let sql = format!("SELECT data FROM {table} WHERE id = $1 AND tenant_id = $2 AND brand_id = $3");
        let row = sqlx::query(&sql).bind(id).bind(&s.tenant_id).bind(&s.brand_id).fetch_optional(&self.pool).await?;
        row.map(|r| from_json(r.get("data"))).transpose()
    }
}

#[async_trait]
impl Store for PgStore {
    async fn create_job(&self, job: Job) -> ApiResult<(Job, bool)> {
        let data = to_json(&job)?;
        let res = sqlx::query(
            "INSERT INTO ingestion_jobs (id, tenant_id, brand_id, actor_id, status, source_type, requested_channels,
                idempotency_key, created_at, updated_at, completed_at, error, data)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)
             ON CONFLICT (tenant_id, brand_id, idempotency_key) WHERE idempotency_key IS NOT NULL DO NOTHING",
        )
        .bind(&job.id)
        .bind(&job.tenant_id)
        .bind(&job.brand_id)
        .bind(&job.actor_id)
        .bind(job.state.as_str())
        .bind(&job.source_type)
        .bind(&job.requested_channels)
        .bind(&job.idempotency_key)
        .bind(job.created_at)
        .bind(job.updated_at)
        .bind(job.completed_at)
        .bind(&job.error)
        .bind(data)
        .execute(&self.pool)
        .await?;
        if res.rows_affected() == 1 {
            return Ok((job, true));
        }
        let row = sqlx::query("SELECT data FROM ingestion_jobs WHERE tenant_id=$1 AND brand_id=$2 AND idempotency_key=$3")
            .bind(&job.tenant_id)
            .bind(&job.brand_id)
            .bind(&job.idempotency_key)
            .fetch_one(&self.pool)
            .await?;
        Ok((from_json(row.get("data"))?, false))
    }

    async fn get_job(&self, s: &Scope, id: &str) -> ApiResult<Option<Job>> {
        self.get_one("ingestion_jobs", s, id).await
    }

    async fn save_job(&self, job: &Job, expected: WorkflowState) -> ApiResult<()> {
        let res = sqlx::query(
            "UPDATE ingestion_jobs SET status=$4, source_type=$5, updated_at=$6, completed_at=$7, error=$8, data=$9
             WHERE id=$1 AND tenant_id=$2 AND brand_id=$3 AND status=$10",
        )
        .bind(&job.id)
        .bind(&job.tenant_id)
        .bind(&job.brand_id)
        .bind(job.state.as_str())
        .bind(&job.source_type)
        .bind(job.updated_at)
        .bind(job.completed_at)
        .bind(&job.error)
        .bind(to_json(job)?)
        .bind(expected.as_str())
        .execute(&self.pool)
        .await?;
        if res.rows_affected() == 0 {
            return Err(ApiError::invalid_state("job missing or modified concurrently"));
        }
        Ok(())
    }

    async fn insert_source(&self, x: Source) -> ApiResult<()> {
        sqlx::query(
            "INSERT INTO sources (id, job_id, tenant_id, brand_id, url, source_type, extraction_method, extraction_status,
                title, author, published_at, language, raw_content_url, normalized_content, metadata, screenshot_url,
                content_hash, created_at, data)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19)",
        )
        .bind(&x.id)
        .bind(&x.job_id)
        .bind(&x.tenant_id)
        .bind(&x.brand_id)
        .bind(&x.url)
        .bind(&x.source_type)
        .bind(&x.extraction_method)
        .bind(&x.extraction_status)
        .bind(&x.title)
        .bind(&x.author)
        .bind(x.published_at)
        .bind(&x.language)
        .bind(&x.raw_content_url)
        .bind(&x.normalized_content)
        .bind(&x.metadata)
        .bind(&x.screenshot_url)
        .bind(&x.content_hash)
        .bind(x.created_at)
        .bind(to_json(&x)?)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_source(&self, s: &Scope, id: &str) -> ApiResult<Option<Source>> {
        self.get_one("sources", s, id).await
    }

    async fn save_source(&self, x: &Source) -> ApiResult<()> {
        let res = sqlx::query(
            "UPDATE sources SET extraction_method=$4, extraction_status=$5, title=$6, author=$7, published_at=$8,
                language=$9, raw_content_url=$10, normalized_content=$11, metadata=$12, screenshot_url=$13,
                content_hash=$14, data=$15
             WHERE id=$1 AND tenant_id=$2 AND brand_id=$3",
        )
        .bind(&x.id)
        .bind(&x.tenant_id)
        .bind(&x.brand_id)
        .bind(&x.extraction_method)
        .bind(&x.extraction_status)
        .bind(&x.title)
        .bind(&x.author)
        .bind(x.published_at)
        .bind(&x.language)
        .bind(&x.raw_content_url)
        .bind(&x.normalized_content)
        .bind(&x.metadata)
        .bind(&x.screenshot_url)
        .bind(&x.content_hash)
        .bind(to_json(x)?)
        .execute(&self.pool)
        .await?;
        if res.rows_affected() == 0 {
            return Err(ApiError::not_found("source", &x.id));
        }
        Ok(())
    }

    async fn list_sources(&self, s: &Scope, job_id: &str) -> ApiResult<Vec<Source>> {
        let rows = sqlx::query("SELECT data FROM sources WHERE tenant_id=$1 AND brand_id=$2 AND job_id=$3 ORDER BY created_at")
            .bind(&s.tenant_id)
            .bind(&s.brand_id)
            .bind(job_id)
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(|r| from_json(r.get("data"))).collect()
    }

    async fn find_duplicate_source(&self, s: &Scope, url: &str, hash: Option<&str>, exclude: Option<&str>) -> ApiResult<Option<Source>> {
        let row = sqlx::query(
            "SELECT data FROM sources
             WHERE tenant_id=$1 AND brand_id=$2 AND extraction_status='success'
               AND ($5::text IS NULL OR id <> $5)
               AND (url=$3 OR ($4::text IS NOT NULL AND content_hash=$4))
             ORDER BY created_at LIMIT 1",
        )
        .bind(&s.tenant_id)
        .bind(&s.brand_id)
        .bind(url)
        .bind(hash)
        .bind(exclude)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| from_json(r.get("data"))).transpose()
    }

    async fn insert_report(&self, r: Report) -> ApiResult<()> {
        sqlx::query(
            "INSERT INTO content_reports (id, job_id, tenant_id, brand_id, source_ids, report_payload, model_provider,
                model_version, prompt_version, created_at, data)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
        )
        .bind(&r.id)
        .bind(&r.job_id)
        .bind(&r.tenant_id)
        .bind(&r.brand_id)
        .bind(&r.source_ids)
        .bind(to_json(&r.payload)?)
        .bind(&r.model.provider)
        .bind(&r.model.model)
        .bind(&r.model.prompt_version)
        .bind(r.created_at)
        .bind(to_json(&r)?)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_report(&self, s: &Scope, id: &str) -> ApiResult<Option<Report>> {
        self.get_one("content_reports", s, id).await
    }

    async fn insert_draft(&self, d: Draft) -> ApiResult<()> {
        sqlx::query(
            "INSERT INTO content_drafts (id, job_id, report_id, tenant_id, brand_id, channel, format, status, content_payload,
                media_assets, source_links, compliance_status, created_at, updated_at, data)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)",
        )
        .bind(&d.id)
        .bind(&d.job_id)
        .bind(&d.report_id)
        .bind(&d.tenant_id)
        .bind(&d.brand_id)
        .bind(&d.channel)
        .bind(&d.format)
        .bind(d.state.as_str())
        .bind(to_json(&d.content)?)
        .bind(to_json(&d.media_assets)?)
        .bind(to_json(&d.source_links)?)
        .bind(&d.compliance.status)
        .bind(d.created_at)
        .bind(d.updated_at)
        .bind(to_json(&d)?)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_draft(&self, s: &Scope, id: &str) -> ApiResult<Option<Draft>> {
        self.get_one("content_drafts", s, id).await
    }

    async fn save_draft(&self, d: &Draft, expected: WorkflowState) -> ApiResult<()> {
        let res = sqlx::query(
            "UPDATE content_drafts SET channel=$4, format=$5, status=$6, content_payload=$7, media_assets=$8,
                source_links=$9, compliance_status=$10, updated_at=$11, data=$12
             WHERE id=$1 AND tenant_id=$2 AND brand_id=$3 AND status=$13",
        )
        .bind(&d.id)
        .bind(&d.tenant_id)
        .bind(&d.brand_id)
        .bind(&d.channel)
        .bind(&d.format)
        .bind(d.state.as_str())
        .bind(to_json(&d.content)?)
        .bind(to_json(&d.media_assets)?)
        .bind(to_json(&d.source_links)?)
        .bind(&d.compliance.status)
        .bind(d.updated_at)
        .bind(to_json(d)?)
        .bind(expected.as_str())
        .execute(&self.pool)
        .await?;
        if res.rows_affected() == 0 {
            return Err(ApiError::invalid_state("draft missing or modified concurrently"));
        }
        Ok(())
    }

    async fn list_brand_drafts(&self, s: &Scope, limit: usize) -> ApiResult<Vec<Draft>> {
        let rows = sqlx::query("SELECT data FROM content_drafts WHERE tenant_id=$1 AND brand_id=$2 ORDER BY created_at DESC LIMIT $3")
            .bind(&s.tenant_id)
            .bind(&s.brand_id)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(|r| from_json(r.get("data"))).collect()
    }

    async fn insert_review_event(&self, e: ReviewEvent) -> ApiResult<()> {
        sqlx::query(
            "INSERT INTO review_events (id, draft_id, tenant_id, brand_id, reviewer_id, decision, edited_content, notes, created_at, data)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
        )
        .bind(&e.id)
        .bind(&e.draft_id)
        .bind(&e.tenant_id)
        .bind(&e.brand_id)
        .bind(&e.reviewer_id)
        .bind(to_json(&e.decision)?.as_str().unwrap_or_default().to_string())
        .bind(&e.edited_content)
        .bind(&e.notes)
        .bind(e.created_at)
        .bind(to_json(&e)?)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_review_events(&self, s: &Scope, draft_id: &str) -> ApiResult<Vec<ReviewEvent>> {
        let rows = sqlx::query("SELECT data FROM review_events WHERE tenant_id=$1 AND brand_id=$2 AND draft_id=$3 ORDER BY created_at")
            .bind(&s.tenant_id)
            .bind(&s.brand_id)
            .bind(draft_id)
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(|r| from_json(r.get("data"))).collect()
    }

    async fn insert_publish_request(&self, p: PublishRequest) -> ApiResult<()> {
        sqlx::query(
            "INSERT INTO publish_requests (id, draft_id, tenant_id, brand_id, zernio_profile_id, channels, action, scheduled_at,
                zernio_request_payload, zernio_response, status, created_at, updated_at, data)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)",
        )
        .bind(&p.id)
        .bind(&p.draft_id)
        .bind(&p.tenant_id)
        .bind(&p.brand_id)
        .bind(&p.zernio_profile_id)
        .bind(&p.channels)
        .bind(to_json(&p.action)?.as_str().unwrap_or_default().to_string())
        .bind(p.scheduled_at)
        .bind(&p.zernio_request_payload)
        .bind(&p.zernio_response)
        .bind(to_json(&p.status)?.as_str().unwrap_or_default().to_string())
        .bind(p.created_at)
        .bind(p.updated_at)
        .bind(to_json(&p)?)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_publish_request(&self, s: &Scope, id: &str) -> ApiResult<Option<PublishRequest>> {
        self.get_one("publish_requests", s, id).await
    }

    async fn save_publish_request(&self, p: &PublishRequest) -> ApiResult<()> {
        let res = sqlx::query(
            "UPDATE publish_requests SET zernio_request_payload=$4, zernio_response=$5, status=$6, updated_at=$7, data=$8
             WHERE id=$1 AND tenant_id=$2 AND brand_id=$3",
        )
        .bind(&p.id)
        .bind(&p.tenant_id)
        .bind(&p.brand_id)
        .bind(&p.zernio_request_payload)
        .bind(&p.zernio_response)
        .bind(to_json(&p.status)?.as_str().unwrap_or_default().to_string())
        .bind(p.updated_at)
        .bind(to_json(p)?)
        .execute(&self.pool)
        .await?;
        if res.rows_affected() == 0 {
            return Err(ApiError::not_found("publish_request", &p.id));
        }
        Ok(())
    }

    async fn list_publish_requests(&self, s: &Scope, draft_id: &str) -> ApiResult<Vec<PublishRequest>> {
        let rows = sqlx::query("SELECT data FROM publish_requests WHERE tenant_id=$1 AND brand_id=$2 AND draft_id=$3 ORDER BY created_at")
            .bind(&s.tenant_id)
            .bind(&s.brand_id)
            .bind(draft_id)
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(|r| from_json(r.get("data"))).collect()
    }

    async fn record_tool_invocation(&self, i: ToolInvocation) -> ApiResult<()> {
        sqlx::query(
            "INSERT INTO tool_invocations (id, tenant_id, brand_id, actor_id, tool_name, input_hash, status, latency_ms,
                token_usage, cost, error, trace_id, created_at)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
        )
        .bind(&i.id)
        .bind(&i.tenant_id)
        .bind(&i.brand_id)
        .bind(&i.actor_id)
        .bind(&i.tool_name)
        .bind(&i.input_hash)
        .bind(&i.status)
        .bind(i.latency_ms as i64)
        .bind(&i.token_usage)
        .bind(i.cost)
        .bind(&i.error)
        .bind(&i.trace_id)
        .bind(i.created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn record_transition(&self, t: Transition) -> ApiResult<()> {
        sqlx::query(
            "INSERT INTO state_transitions (id, tenant_id, brand_id, entity_kind, entity_id, actor_id, from_state, to_state,
                reason, correlation_id, trace_id, at)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
        )
        .bind(&t.id)
        .bind(&t.tenant_id)
        .bind(&t.brand_id)
        .bind(&t.entity_kind)
        .bind(&t.entity_id)
        .bind(&t.actor_id)
        .bind(t.from_state.as_str())
        .bind(t.to_state.as_str())
        .bind(&t.reason)
        .bind(&t.correlation_id)
        .bind(&t.trace_id)
        .bind(t.at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_transitions(&self, s: &Scope, entity_id: &str) -> ApiResult<Vec<Transition>> {
        let rows = sqlx::query(
            "SELECT id, tenant_id, brand_id, entity_kind, entity_id, actor_id, from_state, to_state, reason,
                    correlation_id, trace_id, at
             FROM state_transitions WHERE tenant_id=$1 AND brand_id=$2 AND entity_id=$3 ORDER BY at",
        )
        .bind(&s.tenant_id)
        .bind(&s.brand_id)
        .bind(entity_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|r| {
                let f: String = r.get("from_state");
                let t: String = r.get("to_state");
                Ok(Transition {
                    id: r.get("id"),
                    tenant_id: r.get("tenant_id"),
                    brand_id: r.get("brand_id"),
                    entity_kind: r.get("entity_kind"),
                    entity_id: r.get("entity_id"),
                    actor_id: r.get("actor_id"),
                    from_state: WorkflowState::parse(&f).ok_or_else(|| ApiError::internal("bad from_state"))?,
                    to_state: WorkflowState::parse(&t).ok_or_else(|| ApiError::internal("bad to_state"))?,
                    reason: r.get("reason"),
                    correlation_id: r.get("correlation_id"),
                    trace_id: r.get("trace_id"),
                    at: r.get("at"),
                })
            })
            .collect()
    }

    async fn purge_source_content(&self, cutoff: chrono::DateTime<chrono::Utc>) -> ApiResult<u64> {
        // Keep the typed columns and the jsonb copy in sync.
        let r = sqlx::query(
            "UPDATE sources SET normalized_content=NULL, raw_content_url=NULL, screenshot_url=NULL,
                    data = data - 'normalized_content' - 'raw_content_url' - 'screenshot_url' - 'summary'
             WHERE created_at < $1 AND (normalized_content IS NOT NULL OR screenshot_url IS NOT NULL OR raw_content_url IS NOT NULL)",
        )
        .bind(cutoff)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected())
    }

    async fn ping(&self) -> ApiResult<()> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }
}
