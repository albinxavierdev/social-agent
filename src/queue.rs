//! Durable work queue (spec §9). At-least-once delivery with leases, exponential
//! backoff + full jitter, dead-letter state, and per-job cancellation.
//! `PgQueue` is the production implementation; `MemoryQueue` backs tests.

use crate::context::Scope;
use crate::error::{ApiError, ApiResult};
use crate::ids::{new_id, TRACE};
use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgPool, Row};
use std::sync::Mutex;
use std::time::Duration;

pub const KIND_EXTRACT_SOURCE: &str = "extract_source";
pub const KIND_SUBMIT_ZERNIO: &str = "submit_zernio";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewTask {
    pub kind: String,
    pub tenant_id: String,
    pub brand_id: String,
    pub job_id: Option<String>,
    pub payload: Value,
    pub max_attempts: i32,
    pub correlation_id: String,
}

impl NewTask {
    pub fn new(kind: &str, scope: &Scope, job_id: Option<String>, payload: Value, correlation_id: &str) -> Self {
        Self {
            kind: kind.into(),
            tenant_id: scope.tenant_id.clone(),
            brand_id: scope.brand_id.clone(),
            job_id,
            payload,
            max_attempts: 5,
            correlation_id: correlation_id.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Task {
    pub id: String,
    pub kind: String,
    pub tenant_id: String,
    pub brand_id: String,
    pub job_id: Option<String>,
    pub payload: Value,
    pub attempts: i32,
    pub max_attempts: i32,
    pub correlation_id: String,
    pub trace_id: String,
}

impl Task {
    pub fn scope(&self) -> Scope {
        Scope { tenant_id: self.tenant_id.clone(), brand_id: self.brand_id.clone() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailOutcome {
    Retrying { run_after: DateTime<Utc>, attempt: i32 },
    DeadLettered,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct QueueStats {
    pub pending: i64,
    pub leased: i64,
    pub dead: i64,
    pub oldest_pending_age_secs: i64,
}

#[async_trait]
pub trait Queue: Send + Sync + 'static {
    async fn enqueue(&self, t: NewTask) -> ApiResult<String>;
    /// Lease the next ready task. Leased tasks that are not completed before
    /// `lease_for` elapses become available again (crash recovery).
    async fn lease(&self, lease_for: Duration) -> ApiResult<Option<Task>>;
    async fn complete(&self, task_id: &str) -> ApiResult<()>;
    async fn fail(&self, task_id: &str, error: &str, retryable: bool, backoff: Duration) -> ApiResult<FailOutcome>;
    async fn cancel_job(&self, scope: &Scope, job_id: &str) -> ApiResult<u64>;
    async fn stats(&self) -> ApiResult<QueueStats>;
}

/// Exponential backoff with full jitter: uniform in [0, min(cap, base * 2^attempt)].
pub fn backoff_delay(attempt: u32, base: Duration, cap: Duration) -> Duration {
    let exp = base.saturating_mul(1u32.checked_shl(attempt.min(20)).unwrap_or(u32::MAX));
    let ceiling = exp.min(cap);
    let ms = rand::thread_rng().gen_range(0..=ceiling.as_millis().max(1) as u64);
    Duration::from_millis(ms)
}

/// Deterministic ceiling used by tests and for documentation.
pub fn backoff_ceiling(attempt: u32, base: Duration, cap: Duration) -> Duration {
    base.saturating_mul(1u32.checked_shl(attempt.min(20)).unwrap_or(u32::MAX)).min(cap)
}

// ---------------------------------------------------------------- memory

#[derive(Debug, Clone, PartialEq, Eq)]
enum MState {
    Pending,
    Leased(DateTime<Utc>),
    Done,
    Dead,
    Cancelled,
}

struct MTask {
    task: Task,
    state: MState,
    run_after: DateTime<Utc>,
    created_at: DateTime<Utc>,
    last_error: Option<String>,
}

#[derive(Default)]
pub struct MemoryQueue {
    tasks: Mutex<Vec<MTask>>,
}

impl MemoryQueue {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn dead_letters(&self) -> Vec<(String, Option<String>)> {
        self.tasks.lock().unwrap().iter().filter(|t| t.state == MState::Dead).map(|t| (t.task.id.clone(), t.last_error.clone())).collect()
    }
    pub fn make_all_ready(&self) {
        for t in self.tasks.lock().unwrap().iter_mut() {
            t.run_after = Utc::now() - ChronoDuration::seconds(1);
        }
    }
}

#[async_trait]
impl Queue for MemoryQueue {
    async fn enqueue(&self, n: NewTask) -> ApiResult<String> {
        let id = new_id("task");
        let now = Utc::now();
        self.tasks.lock().unwrap().push(MTask {
            task: Task {
                id: id.clone(),
                kind: n.kind,
                tenant_id: n.tenant_id,
                brand_id: n.brand_id,
                job_id: n.job_id,
                payload: n.payload,
                attempts: 0,
                max_attempts: n.max_attempts,
                correlation_id: n.correlation_id,
                trace_id: new_id(TRACE),
            },
            state: MState::Pending,
            run_after: now,
            created_at: now,
            last_error: None,
        });
        Ok(id)
    }

    async fn lease(&self, lease_for: Duration) -> ApiResult<Option<Task>> {
        let now = Utc::now();
        let mut g = self.tasks.lock().unwrap();
        for t in g.iter_mut() {
            let expired = matches!(t.state, MState::Leased(until) if until <= now);
            if (t.state == MState::Pending || expired) && t.run_after <= now {
                t.task.attempts += 1;
                t.state = MState::Leased(now + ChronoDuration::from_std(lease_for).unwrap_or_default());
                return Ok(Some(t.task.clone()));
            }
        }
        Ok(None)
    }

    async fn complete(&self, id: &str) -> ApiResult<()> {
        let mut g = self.tasks.lock().unwrap();
        if let Some(t) = g.iter_mut().find(|t| t.task.id == id) {
            if t.state != MState::Cancelled {
                t.state = MState::Done;
            }
        }
        Ok(())
    }

    async fn fail(&self, id: &str, error: &str, retryable: bool, backoff: Duration) -> ApiResult<FailOutcome> {
        let mut g = self.tasks.lock().unwrap();
        let t = g.iter_mut().find(|t| t.task.id == id).ok_or_else(|| ApiError::not_found("task", id))?;
        t.last_error = Some(error.to_string());
        if !retryable || t.task.attempts >= t.task.max_attempts {
            t.state = MState::Dead;
            return Ok(FailOutcome::DeadLettered);
        }
        let run_after = Utc::now() + ChronoDuration::from_std(backoff).unwrap_or_default();
        t.state = MState::Pending;
        t.run_after = run_after;
        Ok(FailOutcome::Retrying { run_after, attempt: t.task.attempts })
    }

    async fn cancel_job(&self, s: &Scope, job_id: &str) -> ApiResult<u64> {
        let mut n = 0;
        for t in self.tasks.lock().unwrap().iter_mut() {
            if t.task.job_id.as_deref() == Some(job_id)
                && t.task.tenant_id == s.tenant_id
                && t.task.brand_id == s.brand_id
                && matches!(t.state, MState::Pending | MState::Leased(_))
            {
                t.state = MState::Cancelled;
                n += 1;
            }
        }
        Ok(n)
    }

    async fn stats(&self) -> ApiResult<QueueStats> {
        let g = self.tasks.lock().unwrap();
        let now = Utc::now();
        let mut s = QueueStats::default();
        for t in g.iter() {
            match t.state {
                MState::Pending => {
                    s.pending += 1;
                    s.oldest_pending_age_secs = s.oldest_pending_age_secs.max((now - t.created_at).num_seconds());
                }
                MState::Leased(_) => s.leased += 1,
                MState::Dead => s.dead += 1,
                _ => {}
            }
        }
        Ok(s)
    }
}

// -------------------------------------------------------------- postgres

pub struct PgQueue {
    pool: PgPool,
}

impl PgQueue {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl Queue for PgQueue {
    async fn enqueue(&self, n: NewTask) -> ApiResult<String> {
        let id = new_id("task");
        sqlx::query(
            "INSERT INTO queue_tasks (id, kind, tenant_id, brand_id, job_id, payload, max_attempts, correlation_id, trace_id)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
        )
        .bind(&id)
        .bind(&n.kind)
        .bind(&n.tenant_id)
        .bind(&n.brand_id)
        .bind(&n.job_id)
        .bind(&n.payload)
        .bind(n.max_attempts)
        .bind(&n.correlation_id)
        .bind(new_id(TRACE))
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    async fn lease(&self, lease_for: Duration) -> ApiResult<Option<Task>> {
        // SKIP LOCKED lets many workers poll concurrently without double-leasing.
        let row = sqlx::query(
            "UPDATE queue_tasks SET state='leased', attempts = attempts + 1, updated_at = now(),
                    lease_until = now() + ($1 || ' milliseconds')::interval
             WHERE id = (
                SELECT id FROM queue_tasks
                WHERE (state='pending' AND run_after <= now()) OR (state='leased' AND lease_until <= now())
                ORDER BY run_after FOR UPDATE SKIP LOCKED LIMIT 1)
             RETURNING id, kind, tenant_id, brand_id, job_id, payload, attempts, max_attempts, correlation_id, trace_id",
        )
        .bind(lease_for.as_millis().to_string())
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| Task {
            id: r.get("id"),
            kind: r.get("kind"),
            tenant_id: r.get("tenant_id"),
            brand_id: r.get("brand_id"),
            job_id: r.get("job_id"),
            payload: r.get("payload"),
            attempts: r.get("attempts"),
            max_attempts: r.get("max_attempts"),
            correlation_id: r.get("correlation_id"),
            trace_id: r.get("trace_id"),
        }))
    }

    async fn complete(&self, id: &str) -> ApiResult<()> {
        sqlx::query("UPDATE queue_tasks SET state='done', updated_at=now() WHERE id=$1 AND state='leased'").bind(id).execute(&self.pool).await?;
        Ok(())
    }

    async fn fail(&self, id: &str, error: &str, retryable: bool, backoff: Duration) -> ApiResult<FailOutcome> {
        let row = sqlx::query(
            "UPDATE queue_tasks SET last_error=$2, updated_at=now(),
                state = CASE WHEN $3 AND attempts < max_attempts THEN 'pending' ELSE 'dead' END,
                run_after = CASE WHEN $3 AND attempts < max_attempts
                                 THEN now() + ($4 || ' milliseconds')::interval ELSE run_after END,
                lease_until = NULL
             WHERE id=$1 RETURNING state, attempts, run_after",
        )
        .bind(id)
        .bind(error)
        .bind(retryable)
        .bind(backoff.as_millis().to_string())
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| ApiError::not_found("task", id))?;
        let state: String = row.get("state");
        Ok(if state == "dead" { FailOutcome::DeadLettered } else { FailOutcome::Retrying { run_after: row.get("run_after"), attempt: row.get("attempts") } })
    }

    async fn cancel_job(&self, s: &Scope, job_id: &str) -> ApiResult<u64> {
        let r = sqlx::query(
            "UPDATE queue_tasks SET state='cancelled', updated_at=now()
             WHERE tenant_id=$1 AND brand_id=$2 AND job_id=$3 AND state IN ('pending','leased')",
        )
        .bind(&s.tenant_id)
        .bind(&s.brand_id)
        .bind(job_id)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected())
    }

    async fn stats(&self) -> ApiResult<QueueStats> {
        let r = sqlx::query(
            "SELECT count(*) FILTER (WHERE state='pending') AS pending,
                    count(*) FILTER (WHERE state='leased') AS leased,
                    count(*) FILTER (WHERE state='dead') AS dead,
                    COALESCE(EXTRACT(EPOCH FROM (now() - min(created_at) FILTER (WHERE state='pending')))::bigint, 0) AS age
             FROM queue_tasks",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(QueueStats { pending: r.get("pending"), leased: r.get("leased"), dead: r.get("dead"), oldest_pending_age_secs: r.get("age") })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> Scope {
        Scope { tenant_id: "t1".into(), brand_id: "b1".into() }
    }

    fn task() -> NewTask {
        NewTask::new(KIND_EXTRACT_SOURCE, &scope(), Some("job_1".into()), serde_json::json!({}), "corr")
    }

    #[test]
    fn backoff_is_bounded_and_grows() {
        let base = Duration::from_millis(100);
        let cap = Duration::from_secs(2);
        assert_eq!(backoff_ceiling(0, base, cap), Duration::from_millis(100));
        assert_eq!(backoff_ceiling(3, base, cap), Duration::from_millis(800));
        assert_eq!(backoff_ceiling(10, base, cap), cap);
        for a in 0..12 {
            assert!(backoff_delay(a, base, cap) <= cap);
        }
    }

    #[tokio::test]
    async fn retries_then_dead_letters() {
        let q = MemoryQueue::new();
        let mut n = task();
        n.max_attempts = 2;
        q.enqueue(n).await.unwrap();
        let t = q.lease(Duration::from_secs(30)).await.unwrap().unwrap();
        assert!(matches!(q.fail(&t.id, "boom", true, Duration::ZERO).await.unwrap(), FailOutcome::Retrying { .. }));
        let t = q.lease(Duration::from_secs(30)).await.unwrap().unwrap();
        assert_eq!(t.attempts, 2);
        assert_eq!(q.fail(&t.id, "boom", true, Duration::ZERO).await.unwrap(), FailOutcome::DeadLettered);
        assert_eq!(q.dead_letters().len(), 1);
        assert!(q.lease(Duration::from_secs(30)).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn non_retryable_goes_straight_to_dlq() {
        let q = MemoryQueue::new();
        q.enqueue(task()).await.unwrap();
        let t = q.lease(Duration::from_secs(30)).await.unwrap().unwrap();
        assert_eq!(q.fail(&t.id, "bad input", false, Duration::ZERO).await.unwrap(), FailOutcome::DeadLettered);
    }

    #[tokio::test]
    async fn cancel_is_scoped_to_tenant_and_job() {
        let q = MemoryQueue::new();
        q.enqueue(task()).await.unwrap();
        let other = Scope { tenant_id: "t2".into(), brand_id: "b1".into() };
        assert_eq!(q.cancel_job(&other, "job_1").await.unwrap(), 0);
        assert_eq!(q.cancel_job(&scope(), "job_1").await.unwrap(), 1);
        assert!(q.lease(Duration::from_secs(1)).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn expired_lease_is_redelivered() {
        let q = MemoryQueue::new();
        q.enqueue(task()).await.unwrap();
        let t = q.lease(Duration::from_millis(1)).await.unwrap().unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        let again = q.lease(Duration::from_secs(30)).await.unwrap().unwrap();
        assert_eq!(t.id, again.id);
        assert_eq!(again.attempts, 2);
    }
}
