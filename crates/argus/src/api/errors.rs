// Rust guideline compliant 2026-02-21
//! API error types for the supervisor REST endpoints.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

/// Errors returned by API route handlers.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// The requested action ID does not exist.
    #[error("action not found: {action_id}")]
    ActionNotFound {
        /// The action ID that was looked up.
        action_id: String,
    },

    /// The agent is already in the requested state.
    #[error("agent is already {state}")]
    AlreadyInState {
        /// Current state description.
        state: &'static str,
    },

    /// The rule index is out of bounds.
    #[error("rule index {index} out of bounds (total: {total})")]
    RuleIndexOutOfBounds {
        /// The requested index.
        index: usize,
        /// Total number of rules.
        total: usize,
    },

    /// The request body could not be parsed.
    #[error("invalid request body: {reason}")]
    InvalidBody {
        /// Description of the problem.
        reason: String,
    },

    /// No tree hash found for the given sequence number.
    #[error("no tree hash for seq {seq}")]
    SeqNotFound {
        /// The sequence number that was looked up.
        seq: u64,
    },

    /// Restore operation failed.
    #[error("restore failed: {reason}")]
    RestoreFailed {
        /// Description of the failure.
        reason: String,
    },
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            ApiError::ActionNotFound { .. } => (StatusCode::NOT_FOUND, self.to_string()),
            ApiError::AlreadyInState { .. } => (StatusCode::CONFLICT, self.to_string()),
            ApiError::RuleIndexOutOfBounds { .. } => (StatusCode::NOT_FOUND, self.to_string()),
            ApiError::InvalidBody { .. } => (StatusCode::BAD_REQUEST, self.to_string()),
            ApiError::SeqNotFound { .. } => (StatusCode::NOT_FOUND, self.to_string()),
            ApiError::RestoreFailed { .. } => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
        };

        let body = axum::Json(json!({ "error": message }));
        (status, body).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_not_found_display() {
        let err = ApiError::ActionNotFound {
            action_id: "abc-123".into(),
        };
        assert!(err.to_string().contains("abc-123"));
    }

    #[test]
    fn already_in_state_display() {
        let err = ApiError::AlreadyInState { state: "paused" };
        assert!(err.to_string().contains("paused"));
    }
}
