//! Stable reason codes shared by local-summary producers and consumers.
//!
//! The local-summary API still returns `String` errors for command
//! compatibility.  Every new error contains one of the stable codes in
//! [`SummaryFailureReason`], so callers do not have to parse human-oriented
//! diagnostics or depend on error wording.

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SummaryFailureReason {
    InvalidModelOutput,
    MetadataEcho,
    UngroundedSummary,
    EmptySource,
    SourceTooLong,
    SummaryRuntimeTimeout,
    SummaryRuntimeFailed,
    SummaryQueueFailed,
    /// Compatibility code for an error that did not match a known category.
    InferenceFailed,
}

/// Short alias for callers that refer to these values as reason codes.
pub type SummaryReasonCode = SummaryFailureReason;

impl SummaryFailureReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidModelOutput => "invalid_model_output",
            Self::MetadataEcho => "metadata_echo",
            Self::UngroundedSummary => "ungrounded_summary",
            Self::EmptySource => "empty_source",
            Self::SourceTooLong => "source_too_long",
            Self::SummaryRuntimeTimeout => "summary_runtime_timeout",
            Self::SummaryRuntimeFailed => "summary_runtime_failed",
            Self::SummaryQueueFailed => "summary_queue_failed",
            Self::InferenceFailed => "inference_failed",
        }
    }

    pub const fn is_contract_violation(self) -> bool {
        matches!(
            self,
            Self::InvalidModelOutput | Self::MetadataEcho | Self::UngroundedSummary
        )
    }

    pub const fn is_runtime_failure(self) -> bool {
        matches!(
            self,
            Self::SummaryRuntimeTimeout | Self::SummaryRuntimeFailed | Self::SummaryQueueFailed
        )
    }
}

impl fmt::Display for SummaryFailureReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Render a machine-readable reason followed by an optional safe diagnostic.
/// Callers must only provide redacted/metadata diagnostics; model content is
/// deliberately not accepted or inferred here.
pub fn format_reason(reason: SummaryFailureReason, detail: &str) -> String {
    let detail = detail.trim();
    if detail.is_empty() {
        reason.as_str().to_string()
    } else {
        format!("{}: {}", reason.as_str(), detail)
    }
}

/// Render a contract error while retaining the historical prefix used by
/// retry/fallback callers.
pub fn format_contract_violation(reason: SummaryFailureReason, detail: &str) -> String {
    debug_assert!(reason.is_contract_violation());
    let rendered = format_reason(reason, detail);
    format!("contract violation: {rendered}")
}

/// Extract a known reason from a compatibility error string.  This helper is
/// intentionally conservative: unknown text remains `inference_failed`, and
/// no error text is copied into a reason code.
pub fn reason_from_error(error: &str) -> SummaryFailureReason {
    let lower = error.to_ascii_lowercase();
    for reason in [
        SummaryFailureReason::MetadataEcho,
        SummaryFailureReason::UngroundedSummary,
        SummaryFailureReason::InvalidModelOutput,
        SummaryFailureReason::EmptySource,
        SummaryFailureReason::SourceTooLong,
        SummaryFailureReason::SummaryQueueFailed,
        SummaryFailureReason::SummaryRuntimeTimeout,
        SummaryFailureReason::SummaryRuntimeFailed,
        SummaryFailureReason::InferenceFailed,
    ] {
        if lower.contains(reason.as_str()) {
            return reason;
        }
    }

    if lower.contains("metadata echo") {
        return SummaryFailureReason::MetadataEcho;
    }
    if lower.contains("ungrounded summary") {
        return SummaryFailureReason::UngroundedSummary;
    }
    if lower.contains("empty source") || lower.contains("invalid_summary_input") {
        return SummaryFailureReason::EmptySource;
    }
    if lower.contains("source too long") {
        return SummaryFailureReason::SourceTooLong;
    }
    if lower.contains("queue full")
        || lower.contains("queue is not running")
        || lower.contains("summary queue failed")
    {
        return SummaryFailureReason::SummaryQueueFailed;
    }
    if lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("summary runtime timeout")
    {
        return SummaryFailureReason::SummaryRuntimeTimeout;
    }
    if lower.contains("contract violation")
        || lower.contains("summary response")
        || lower.contains("should_store")
        || lower.contains("finish_reason")
        || lower.contains("json")
    {
        return SummaryFailureReason::InvalidModelOutput;
    }
    if lower.contains("summary request")
        || lower.contains("summary server")
        || lower.contains("read summary")
        || lower.contains("summary worker")
        || lower.contains("llama-server")
        || lower.contains("server is unavailable")
        || lower.contains("summary runtime failed")
    {
        return SummaryFailureReason::SummaryRuntimeFailed;
    }
    SummaryFailureReason::InferenceFailed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reason_codes_are_stable_snake_case_values() {
        assert_eq!(
            SummaryFailureReason::InvalidModelOutput.as_str(),
            "invalid_model_output"
        );
        assert_eq!(
            SummaryFailureReason::SummaryQueueFailed.to_string(),
            "summary_queue_failed"
        );
        assert!(SummaryFailureReason::MetadataEcho.is_contract_violation());
        assert!(SummaryFailureReason::SummaryRuntimeTimeout.is_runtime_failure());
    }

    #[test]
    fn compatibility_errors_are_classified_without_copying_details() {
        assert_eq!(
            reason_from_error("contract violation: metadata_echo"),
            SummaryFailureReason::MetadataEcho
        );
        assert_eq!(
            reason_from_error("summary request failed: timeout"),
            SummaryFailureReason::SummaryRuntimeTimeout
        );
        assert_eq!(
            reason_from_error("unknown operation"),
            SummaryFailureReason::InferenceFailed
        );
    }
}
