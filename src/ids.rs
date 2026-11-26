//! Prefixed, opaque identifiers (`job_…`, `src_…`, `draft_…`).

use uuid::Uuid;

pub fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::new_v4().simple())
}

pub const JOB: &str = "job";
pub const SOURCE: &str = "src";
pub const REPORT: &str = "report";
pub const DRAFT: &str = "draft";
pub const CAMPAIGN: &str = "camp";
pub const IMAGE: &str = "img";
pub const PUBREQ: &str = "pubreq";
pub const REVIEW: &str = "rev";
pub const INVOCATION: &str = "inv";
pub const TRACE: &str = "trace";
