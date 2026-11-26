//! In-memory store for unit/integration tests and local development.
//! Enforces exactly the same scoping and optimistic-concurrency rules as Postgres.

use super::Store;
use crate::context::Scope;
use crate::error::{ApiError, ApiResult};
use crate::model::*;
use crate::workflow::{Transition, WorkflowState};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Default)]
struct Inner {
    jobs: HashMap<String, Job>,
    sources: HashMap<String, Source>,
    reports: HashMap<String, Report>,
    drafts: HashMap<String, Draft>,
    reviews: Vec<ReviewEvent>,
    publish: HashMap<String, PublishRequest>,
    invocations: Vec<ToolInvocation>,
    transitions: Vec<Transition>,
}

#[derive(Default)]
pub struct MemoryStore {
    inner: Mutex<Inner>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn invocations(&self) -> Vec<ToolInvocation> {
        self.inner.lock().unwrap().invocations.clone()
    }
}

fn in_scope(s: &Scope, tenant: &str, brand: &str) -> bool {
    s.tenant_id == tenant && s.brand_id == brand
}

#[async_trait]
impl Store for MemoryStore {
    async fn create_job(&self, job: Job) -> ApiResult<(Job, bool)> {
        let mut g = self.inner.lock().unwrap();
        if let Some(key) = &job.idempotency_key {
            if let Some(existing) =
                g.jobs.values().find(|j| j.tenant_id == job.tenant_id && j.brand_id == job.brand_id && j.idempotency_key.as_ref() == Some(key))
            {
                return Ok((existing.clone(), false));
            }
        }
        g.jobs.insert(job.id.clone(), job.clone());
        Ok((job, true))
    }

    async fn get_job(&self, s: &Scope, id: &str) -> ApiResult<Option<Job>> {
        Ok(self.inner.lock().unwrap().jobs.get(id).filter(|j| in_scope(s, &j.tenant_id, &j.brand_id)).cloned())
    }

    async fn save_job(&self, job: &Job, expected: WorkflowState) -> ApiResult<()> {
        let mut g = self.inner.lock().unwrap();
        let cur = g.jobs.get(&job.id).filter(|j| j.tenant_id == job.tenant_id && j.brand_id == job.brand_id);
        match cur {
            None => Err(ApiError::not_found("job", &job.id)),
            Some(c) if c.state != expected => Err(ApiError::invalid_state("job was modified concurrently")),
            Some(_) => {
                g.jobs.insert(job.id.clone(), job.clone());
                Ok(())
            }
        }
    }

    async fn insert_source(&self, src: Source) -> ApiResult<()> {
        self.inner.lock().unwrap().sources.insert(src.id.clone(), src);
        Ok(())
    }

    async fn get_source(&self, s: &Scope, id: &str) -> ApiResult<Option<Source>> {
        Ok(self.inner.lock().unwrap().sources.get(id).filter(|x| in_scope(s, &x.tenant_id, &x.brand_id)).cloned())
    }

    async fn save_source(&self, src: &Source) -> ApiResult<()> {
        let mut g = self.inner.lock().unwrap();
        match g.sources.get(&src.id) {
            Some(c) if c.tenant_id == src.tenant_id && c.brand_id == src.brand_id => {
                g.sources.insert(src.id.clone(), src.clone());
                Ok(())
            }
            _ => Err(ApiError::not_found("source", &src.id)),
        }
    }

    async fn list_sources(&self, s: &Scope, job_id: &str) -> ApiResult<Vec<Source>> {
        let g = self.inner.lock().unwrap();
        let mut v: Vec<_> = g.sources.values().filter(|x| x.job_id == job_id && in_scope(s, &x.tenant_id, &x.brand_id)).cloned().collect();
        v.sort_by_key(|x| x.created_at);
        Ok(v)
    }

    async fn find_duplicate_source(&self, s: &Scope, url: &str, hash: Option<&str>, exclude: Option<&str>) -> ApiResult<Option<Source>> {
        let g = self.inner.lock().unwrap();
        Ok(g.sources
            .values()
            .filter(|x| in_scope(s, &x.tenant_id, &x.brand_id) && Some(x.id.as_str()) != exclude)
            .find(|x| {
                (x.url == url && x.extraction_status == "success") || (hash.is_some() && x.content_hash.as_deref() == hash && x.extraction_status == "success")
            })
            .cloned())
    }

    async fn insert_report(&self, r: Report) -> ApiResult<()> {
        self.inner.lock().unwrap().reports.insert(r.id.clone(), r);
        Ok(())
    }

    async fn get_report(&self, s: &Scope, id: &str) -> ApiResult<Option<Report>> {
        Ok(self.inner.lock().unwrap().reports.get(id).filter(|x| in_scope(s, &x.tenant_id, &x.brand_id)).cloned())
    }

    async fn insert_draft(&self, d: Draft) -> ApiResult<()> {
        self.inner.lock().unwrap().drafts.insert(d.id.clone(), d);
        Ok(())
    }

    async fn get_draft(&self, s: &Scope, id: &str) -> ApiResult<Option<Draft>> {
        Ok(self.inner.lock().unwrap().drafts.get(id).filter(|x| in_scope(s, &x.tenant_id, &x.brand_id)).cloned())
    }

    async fn save_draft(&self, d: &Draft, expected: WorkflowState) -> ApiResult<()> {
        let mut g = self.inner.lock().unwrap();
        let cur = g.drafts.get(&d.id).filter(|x| x.tenant_id == d.tenant_id && x.brand_id == d.brand_id);
        match cur {
            None => Err(ApiError::not_found("draft", &d.id)),
            Some(c) if c.state != expected => Err(ApiError::invalid_state("draft was modified concurrently")),
            Some(_) => {
                g.drafts.insert(d.id.clone(), d.clone());
                Ok(())
            }
        }
    }

    async fn list_brand_drafts(&self, s: &Scope, limit: usize) -> ApiResult<Vec<Draft>> {
        let g = self.inner.lock().unwrap();
        let mut v: Vec<_> = g.drafts.values().filter(|x| in_scope(s, &x.tenant_id, &x.brand_id)).cloned().collect();
        v.sort_by_key(|x| std::cmp::Reverse(x.created_at));
        v.truncate(limit);
        Ok(v)
    }

    async fn insert_review_event(&self, e: ReviewEvent) -> ApiResult<()> {
        self.inner.lock().unwrap().reviews.push(e);
        Ok(())
    }

    async fn list_review_events(&self, s: &Scope, draft_id: &str) -> ApiResult<Vec<ReviewEvent>> {
        let g = self.inner.lock().unwrap();
        Ok(g.reviews.iter().filter(|e| e.draft_id == draft_id && in_scope(s, &e.tenant_id, &e.brand_id)).cloned().collect())
    }

    async fn insert_publish_request(&self, p: PublishRequest) -> ApiResult<()> {
        self.inner.lock().unwrap().publish.insert(p.id.clone(), p);
        Ok(())
    }

    async fn get_publish_request(&self, s: &Scope, id: &str) -> ApiResult<Option<PublishRequest>> {
        Ok(self.inner.lock().unwrap().publish.get(id).filter(|x| in_scope(s, &x.tenant_id, &x.brand_id)).cloned())
    }

    async fn save_publish_request(&self, p: &PublishRequest) -> ApiResult<()> {
        let mut g = self.inner.lock().unwrap();
        match g.publish.get(&p.id) {
            Some(c) if c.tenant_id == p.tenant_id && c.brand_id == p.brand_id => {
                g.publish.insert(p.id.clone(), p.clone());
                Ok(())
            }
            _ => Err(ApiError::not_found("publish_request", &p.id)),
        }
    }

    async fn list_publish_requests(&self, s: &Scope, draft_id: &str) -> ApiResult<Vec<PublishRequest>> {
        let g = self.inner.lock().unwrap();
        Ok(g.publish.values().filter(|p| p.draft_id == draft_id && in_scope(s, &p.tenant_id, &p.brand_id)).cloned().collect())
    }

    async fn record_tool_invocation(&self, i: ToolInvocation) -> ApiResult<()> {
        self.inner.lock().unwrap().invocations.push(i);
        Ok(())
    }

    async fn record_transition(&self, t: Transition) -> ApiResult<()> {
        self.inner.lock().unwrap().transitions.push(t);
        Ok(())
    }

    async fn list_transitions(&self, s: &Scope, entity_id: &str) -> ApiResult<Vec<Transition>> {
        let g = self.inner.lock().unwrap();
        Ok(g.transitions.iter().filter(|t| t.entity_id == entity_id && in_scope(s, &t.tenant_id, &t.brand_id)).cloned().collect())
    }

    async fn purge_source_content(&self, cutoff: chrono::DateTime<chrono::Utc>) -> ApiResult<u64> {
        let mut g = self.inner.lock().unwrap();
        let mut n = 0;
        for s in g.sources.values_mut().filter(|s| s.created_at < cutoff && s.normalized_content.is_some()) {
            s.normalized_content = None;
            s.raw_content_url = None;
            s.screenshot_url = None;
            s.summary = None;
            n += 1;
        }
        Ok(n)
    }

    async fn ping(&self) -> ApiResult<()> {
        Ok(())
    }
}
