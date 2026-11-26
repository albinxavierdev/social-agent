//! Durable workflow state machine (spec §8) shared by jobs and drafts.

use crate::error::{ApiError, ApiResult};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowState {
    Created,
    Queued,
    Extracting,
    ReportReady,
    DraftReady,
    AwaitingCustomerReview,
    Approved,
    ReadyForZernio,
    SubmittedToZernio,
    ScheduledOrPublished,
    Failed,
    Cancelled,
    Rejected,
    RevisionRequested,
}

impl WorkflowState {
    pub fn as_str(self) -> &'static str {
        use WorkflowState::*;
        match self {
            Created => "created",
            Queued => "queued",
            Extracting => "extracting",
            ReportReady => "report_ready",
            DraftReady => "draft_ready",
            AwaitingCustomerReview => "awaiting_customer_review",
            Approved => "approved",
            ReadyForZernio => "ready_for_zernio",
            SubmittedToZernio => "submitted_to_zernio",
            ScheduledOrPublished => "scheduled_or_published",
            Failed => "failed",
            Cancelled => "cancelled",
            Rejected => "rejected",
            RevisionRequested => "revision_requested",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        use WorkflowState::*;
        Some(match s {
            "created" => Created,
            "queued" => Queued,
            "extracting" => Extracting,
            "report_ready" => ReportReady,
            "draft_ready" => DraftReady,
            "awaiting_customer_review" => AwaitingCustomerReview,
            "approved" => Approved,
            "ready_for_zernio" => ReadyForZernio,
            "submitted_to_zernio" => SubmittedToZernio,
            "scheduled_or_published" => ScheduledOrPublished,
            "failed" => Failed,
            "cancelled" => Cancelled,
            "rejected" => Rejected,
            "revision_requested" => RevisionRequested,
            _ => return None,
        })
    }

    /// Terminal states can never be left.
    pub fn is_terminal(self) -> bool {
        use WorkflowState::*;
        matches!(self, ScheduledOrPublished | Failed | Cancelled | Rejected)
    }

    pub fn can_transition_to(self, next: WorkflowState) -> bool {
        use WorkflowState::*;
        if self.is_terminal() || self == next {
            return false;
        }
        // Any non-terminal state may fail or be cancelled.
        if matches!(next, Failed | Cancelled) {
            return true;
        }
        match (self, next) {
            (Created, Queued | Extracting | DraftReady | ReportReady) => true,
            (Queued, Extracting) => true,
            (Extracting, ReportReady | Queued) => true, // Queued = retry after backoff
            (ReportReady, DraftReady) => true,
            (DraftReady, AwaitingCustomerReview) => true,
            (AwaitingCustomerReview, Approved | Rejected | RevisionRequested) => true,
            // A revised draft goes back through draft generation and review.
            (RevisionRequested, DraftReady | AwaitingCustomerReview) => true,
            (Approved, ReadyForZernio) => true,
            (ReadyForZernio, SubmittedToZernio) => true,
            (SubmittedToZernio, ScheduledOrPublished) => true,
            // Submission can be retried after a transient Zernio failure.
            (SubmittedToZernio, ReadyForZernio) => true,
            _ => false,
        }
    }

    pub fn require_transition(self, next: WorkflowState) -> ApiResult<()> {
        if self.can_transition_to(next) {
            Ok(())
        } else {
            Err(ApiError::invalid_state(format!("illegal transition {} -> {}", self.as_str(), next.as_str()))
                .with_details(serde_json::json!({ "from": self.as_str(), "to": next.as_str() })))
        }
    }
}

/// Persisted record of every transition (spec §8).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transition {
    pub id: String,
    pub tenant_id: String,
    pub brand_id: String,
    pub entity_kind: String, // "job" | "draft"
    pub entity_id: String,
    pub actor_id: String,
    pub from_state: WorkflowState,
    pub to_state: WorkflowState,
    pub reason: String,
    pub correlation_id: String,
    pub trace_id: String,
    pub at: chrono::DateTime<chrono::Utc>,
}

#[cfg(test)]
mod tests {
    use super::WorkflowState::*;

    #[test]
    fn happy_path_is_allowed() {
        let path =
            [Created, Queued, Extracting, ReportReady, DraftReady, AwaitingCustomerReview, Approved, ReadyForZernio, SubmittedToZernio, ScheduledOrPublished];
        for w in path.windows(2) {
            assert!(w[0].can_transition_to(w[1]), "{:?}->{:?}", w[0], w[1]);
        }
    }

    #[test]
    fn cannot_skip_review() {
        assert!(!DraftReady.can_transition_to(Approved));
        assert!(!DraftReady.can_transition_to(ReadyForZernio));
        assert!(!AwaitingCustomerReview.can_transition_to(ReadyForZernio));
        assert!(!Created.can_transition_to(ScheduledOrPublished));
    }

    #[test]
    fn terminal_states_are_final() {
        for t in [ScheduledOrPublished, Failed, Cancelled, Rejected] {
            for n in [Queued, Approved, Failed, Cancelled, DraftReady] {
                assert!(!t.can_transition_to(n), "{:?}->{:?}", t, n);
            }
        }
    }

    #[test]
    fn any_active_state_can_fail_or_cancel() {
        for s in [Created, Queued, Extracting, DraftReady, Approved, SubmittedToZernio] {
            assert!(s.can_transition_to(Failed));
            assert!(s.can_transition_to(Cancelled));
        }
    }

    #[test]
    fn revision_loops_back() {
        assert!(AwaitingCustomerReview.can_transition_to(RevisionRequested));
        assert!(RevisionRequested.can_transition_to(DraftReady));
        assert!(!RevisionRequested.can_transition_to(Approved));
    }

    #[test]
    fn parse_roundtrip() {
        for s in [
            Created,
            Queued,
            Extracting,
            ReportReady,
            DraftReady,
            AwaitingCustomerReview,
            Approved,
            ReadyForZernio,
            SubmittedToZernio,
            ScheduledOrPublished,
            Failed,
            Cancelled,
            Rejected,
            RevisionRequested,
        ] {
            assert_eq!(super::WorkflowState::parse(s.as_str()), Some(s));
        }
    }
}
