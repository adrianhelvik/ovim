//! Versioned, bounded child-to-parent handoffs.
//!
//! Provider output is untrusted. Callers must validate the JSON payload before
//! recording a handoff or treating an agent as complete.

use super::AgentWorkspaceWarning;
use crate::run_log::{ArtifactId, RepoPath};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeSet;
use std::fmt;

pub const STRUCTURED_HANDOFF_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffStatus {
    Completed,
    Failed,
    Interrupted,
    TimedOut,
}

impl HandoffStatus {
    pub fn is_completed(self) -> bool {
        self == Self::Completed
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffConfidence {
    Low,
    Medium,
    High,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffEvidence {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    pub claim: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    Passed,
    Failed,
    Skipped,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffVerification {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    pub status: VerificationStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Provider-facing wire contract. Deserializing this type does not establish
/// trust; use [`HandoffValidator`] to obtain a [`ValidatedHandoff`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructuredHandoffV1 {
    pub version: u32,
    pub status: HandoffStatus,
    pub summary: String,
    #[serde(default)]
    pub evidence: Vec<HandoffEvidence>,
    #[serde(default)]
    pub changed_files: Vec<String>,
    #[serde(default)]
    pub verification: Vec<HandoffVerification>,
    #[serde(default)]
    pub blockers: Vec<String>,
    #[serde(default)]
    pub followups: Vec<String>,
    #[serde(default)]
    pub attachments: Vec<ArtifactId>,
    pub confidence: HandoffConfidence,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HandoffLimits {
    pub max_json_bytes: usize,
    pub max_summary_bytes: usize,
    pub max_text_bytes: usize,
    pub max_path_bytes: usize,
    pub max_command_bytes: usize,
    pub max_evidence: usize,
    pub max_changed_files: usize,
    pub max_verification: usize,
    pub max_blockers: usize,
    pub max_followups: usize,
    pub max_attachments: usize,
}

impl Default for HandoffLimits {
    fn default() -> Self {
        Self {
            max_json_bytes: 64 * 1024,
            max_summary_bytes: 4 * 1024,
            max_text_bytes: 2 * 1024,
            max_path_bytes: 1024,
            max_command_bytes: 4 * 1024,
            max_evidence: 64,
            max_changed_files: 256,
            max_verification: 64,
            max_blockers: 32,
            max_followups: 32,
            max_attachments: 16,
        }
    }
}

#[derive(Clone, Debug)]
pub struct HandoffValidator {
    limits: HandoffLimits,
}

impl HandoffValidator {
    pub fn new(limits: HandoffLimits) -> Self {
        Self { limits }
    }

    pub fn limits(&self) -> HandoffLimits {
        self.limits
    }

    /// Rejects an oversized payload before JSON parsing can allocate strings
    /// from attacker-controlled provider output.
    pub fn validate_json(
        &self,
        bytes: &[u8],
        expected_status: Option<HandoffStatus>,
    ) -> Result<ValidatedHandoff, HandoffValidationError> {
        if bytes.len() > self.limits.max_json_bytes {
            return Err(HandoffValidationError::PayloadTooLarge {
                actual: bytes.len(),
                maximum: self.limits.max_json_bytes,
            });
        }
        let handoff = serde_json::from_slice(bytes)
            .map_err(|error| HandoffValidationError::Malformed(error.to_string()))?;
        self.validate(handoff, expected_status)
    }

    pub fn validate(
        &self,
        handoff: StructuredHandoffV1,
        expected_status: Option<HandoffStatus>,
    ) -> Result<ValidatedHandoff, HandoffValidationError> {
        if handoff.version != STRUCTURED_HANDOFF_VERSION {
            return Err(HandoffValidationError::UnsupportedVersion(handoff.version));
        }
        if let Some(expected) = expected_status
            && handoff.status != expected
        {
            return Err(HandoffValidationError::StatusMismatch {
                expected,
                actual: handoff.status,
            });
        }

        validate_text("summary", &handoff.summary, self.limits.max_summary_bytes)?;
        validate_count("evidence", handoff.evidence.len(), self.limits.max_evidence)?;
        validate_count(
            "changed_files",
            handoff.changed_files.len(),
            self.limits.max_changed_files,
        )?;
        validate_count(
            "verification",
            handoff.verification.len(),
            self.limits.max_verification,
        )?;
        validate_count("blockers", handoff.blockers.len(), self.limits.max_blockers)?;
        validate_count(
            "followups",
            handoff.followups.len(),
            self.limits.max_followups,
        )?;
        validate_count(
            "attachments",
            handoff.attachments.len(),
            self.limits.max_attachments,
        )?;
        let mut attachments = BTreeSet::new();
        for artifact_id in &handoff.attachments {
            if !attachments.insert(artifact_id) {
                return Err(HandoffValidationError::DuplicateAttachment(
                    artifact_id.clone(),
                ));
            }
        }

        for (index, evidence) in handoff.evidence.iter().enumerate() {
            validate_path(
                "evidence",
                index,
                &evidence.path,
                self.limits.max_path_bytes,
            )?;
            if evidence.line == Some(0) {
                return Err(HandoffValidationError::InvalidEvidenceLine { index });
            }
            validate_text(
                "evidence.claim",
                &evidence.claim,
                self.limits.max_text_bytes,
            )?;
        }

        let mut changed_files = BTreeSet::new();
        for (index, path) in handoff.changed_files.iter().enumerate() {
            validate_path("changed_files", index, path, self.limits.max_path_bytes)?;
            if !changed_files.insert(path.as_str()) {
                return Err(HandoffValidationError::DuplicatePath(path.clone()));
            }
        }

        for verification in &handoff.verification {
            validate_text(
                "verification.kind",
                &verification.kind,
                self.limits.max_text_bytes,
            )?;
            if verification.kind == "command"
                && verification
                    .command
                    .as_deref()
                    .is_none_or(|command| command.trim().is_empty())
            {
                return Err(HandoffValidationError::CommandVerificationWithoutCommand);
            }
            if let Some(command) = &verification.command {
                validate_text(
                    "verification.command",
                    command,
                    self.limits.max_command_bytes,
                )?;
            }
            if let Some(detail) = &verification.detail {
                validate_text("verification.detail", detail, self.limits.max_text_bytes)?;
            }
        }
        for blocker in &handoff.blockers {
            validate_text("blockers", blocker, self.limits.max_text_bytes)?;
        }
        for followup in &handoff.followups {
            validate_text("followups", followup, self.limits.max_text_bytes)?;
        }

        match handoff.status {
            HandoffStatus::Completed => {
                if !handoff.blockers.is_empty() {
                    return Err(HandoffValidationError::CompletedWithBlockers);
                }
                if handoff.evidence.is_empty() {
                    return Err(HandoffValidationError::CompletedWithoutEvidence);
                }
                if handoff
                    .verification
                    .iter()
                    .any(|item| item.status == VerificationStatus::Failed)
                {
                    return Err(HandoffValidationError::CompletedWithFailedVerification);
                }
            }
            HandoffStatus::Failed | HandoffStatus::Interrupted | HandoffStatus::TimedOut => {
                if handoff.blockers.is_empty() {
                    return Err(HandoffValidationError::NonCompletionWithoutBlocker(
                        handoff.status,
                    ));
                }
            }
        }

        let serialized_bytes = serde_json::to_vec(&handoff)
            .map_err(|error| HandoffValidationError::Malformed(error.to_string()))?
            .len();
        if serialized_bytes > self.limits.max_json_bytes {
            return Err(HandoffValidationError::PayloadTooLarge {
                actual: serialized_bytes,
                maximum: self.limits.max_json_bytes,
            });
        }

        Ok(ValidatedHandoff { handoff })
    }
}

impl Default for HandoffValidator {
    fn default() -> Self {
        Self::new(HandoffLimits::default())
    }
}

fn validate_count(
    field: &'static str,
    actual: usize,
    maximum: usize,
) -> Result<(), HandoffValidationError> {
    if actual > maximum {
        Err(HandoffValidationError::TooManyItems {
            field,
            actual,
            maximum,
        })
    } else {
        Ok(())
    }
}

fn validate_text(
    field: &'static str,
    value: &str,
    maximum: usize,
) -> Result<(), HandoffValidationError> {
    if value.trim().is_empty() {
        return Err(HandoffValidationError::EmptyField(field));
    }
    if value.len() > maximum {
        return Err(HandoffValidationError::FieldTooLarge {
            field,
            actual: value.len(),
            maximum,
        });
    }
    Ok(())
}

fn validate_path(
    field: &'static str,
    index: usize,
    value: &str,
    maximum: usize,
) -> Result<(), HandoffValidationError> {
    if value.len() > maximum {
        return Err(HandoffValidationError::FieldTooLarge {
            field,
            actual: value.len(),
            maximum,
        });
    }
    RepoPath::parse(value).map_err(|_| HandoffValidationError::InvalidPath {
        field,
        index,
        path: value.into(),
    })?;
    Ok(())
}

/// A handoff whose version, status semantics, bounds, and paths have been
/// checked. Its inner value is intentionally not mutable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ValidatedHandoff {
    handoff: StructuredHandoffV1,
}

impl ValidatedHandoff {
    pub fn as_handoff(&self) -> &StructuredHandoffV1 {
        &self.handoff
    }

    pub fn status(&self) -> HandoffStatus {
        self.handoff.status
    }

    pub fn parent_projection(&self) -> ParentHandoffProjection {
        self.parent_projection_with_workspace_warnings(&[])
    }

    pub fn parent_projection_with_workspace_warnings(
        &self,
        workspace_warnings: &[AgentWorkspaceWarning],
    ) -> ParentHandoffProjection {
        const SUMMARY_BYTES: usize = 1024;
        const TEXT_BYTES: usize = 512;
        const COMMAND_BYTES: usize = 1024;
        const EVIDENCE: usize = 8;
        const CHANGED_FILES: usize = 24;
        const VERIFICATION: usize = 8;
        const BLOCKERS: usize = 8;
        const FOLLOWUPS: usize = 8;
        const WORKSPACE_WARNINGS: usize = 16;

        ParentHandoffProjection {
            version: self.handoff.version,
            status: self.handoff.status,
            summary: truncate_utf8(&self.handoff.summary, SUMMARY_BYTES),
            evidence: self
                .handoff
                .evidence
                .iter()
                .take(EVIDENCE)
                .map(|item| ParentEvidenceProjection {
                    path: item.path.clone(),
                    line: item.line,
                    claim: truncate_utf8(&item.claim, TEXT_BYTES),
                })
                .collect(),
            changed_files: self
                .handoff
                .changed_files
                .iter()
                .take(CHANGED_FILES)
                .cloned()
                .collect(),
            verification: self
                .handoff
                .verification
                .iter()
                .take(VERIFICATION)
                .map(|item| ParentVerificationProjection {
                    kind: truncate_utf8(&item.kind, TEXT_BYTES),
                    command: item
                        .command
                        .as_deref()
                        .map(|command| truncate_utf8(command, COMMAND_BYTES)),
                    status: item.status,
                    detail: item
                        .detail
                        .as_deref()
                        .map(|detail| truncate_utf8(detail, TEXT_BYTES)),
                })
                .collect(),
            blockers: project_text(&self.handoff.blockers, BLOCKERS, TEXT_BYTES),
            followups: project_text(&self.handoff.followups, FOLLOWUPS, TEXT_BYTES),
            attachments: self.handoff.attachments.clone(),
            omitted_evidence: self.handoff.evidence.len().saturating_sub(EVIDENCE),
            omitted_changed_files: self
                .handoff
                .changed_files
                .len()
                .saturating_sub(CHANGED_FILES),
            omitted_verification: self.handoff.verification.len().saturating_sub(VERIFICATION),
            omitted_blockers: self.handoff.blockers.len().saturating_sub(BLOCKERS),
            omitted_followups: self.handoff.followups.len().saturating_sub(FOLLOWUPS),
            workspace_warnings: workspace_warnings
                .iter()
                .take(WORKSPACE_WARNINGS)
                .cloned()
                .collect(),
            omitted_workspace_warnings: workspace_warnings.len().saturating_sub(WORKSPACE_WARNINGS),
            confidence: self.handoff.confidence,
        }
    }
}

impl<'de> Deserialize<'de> for ValidatedHandoff {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let handoff = StructuredHandoffV1::deserialize(deserializer)?;
        HandoffValidator::default()
            .validate(handoff, None)
            .map_err(serde::de::Error::custom)
    }
}

fn project_text(values: &[String], maximum: usize, bytes: usize) -> Vec<String> {
    values
        .iter()
        .take(maximum)
        .map(|value| truncate_utf8(value, bytes))
        .collect()
}

fn truncate_utf8(value: &str, maximum: usize) -> String {
    if value.len() <= maximum {
        return value.into();
    }
    let suffix = "\u{2026}";
    let mut end = maximum.saturating_sub(suffix.len()).min(value.len());
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}{}", &value[..end], suffix)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParentHandoffProjection {
    pub version: u32,
    pub status: HandoffStatus,
    pub summary: String,
    pub evidence: Vec<ParentEvidenceProjection>,
    pub changed_files: Vec<String>,
    pub verification: Vec<ParentVerificationProjection>,
    pub blockers: Vec<String>,
    pub followups: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<ArtifactId>,
    pub omitted_evidence: usize,
    pub omitted_changed_files: usize,
    pub omitted_verification: usize,
    pub omitted_blockers: usize,
    pub omitted_followups: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_warnings: Vec<AgentWorkspaceWarning>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub omitted_workspace_warnings: usize,
    pub confidence: HandoffConfidence,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParentEvidenceProjection {
    pub path: String,
    pub line: Option<u32>,
    pub claim: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParentVerificationProjection {
    pub kind: String,
    pub command: Option<String>,
    pub status: VerificationStatus,
    pub detail: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HandoffValidationError {
    Malformed(String),
    PayloadTooLarge {
        actual: usize,
        maximum: usize,
    },
    UnsupportedVersion(u32),
    StatusMismatch {
        expected: HandoffStatus,
        actual: HandoffStatus,
    },
    EmptyField(&'static str),
    FieldTooLarge {
        field: &'static str,
        actual: usize,
        maximum: usize,
    },
    TooManyItems {
        field: &'static str,
        actual: usize,
        maximum: usize,
    },
    InvalidPath {
        field: &'static str,
        index: usize,
        path: String,
    },
    DuplicatePath(String),
    DuplicateAttachment(ArtifactId),
    InvalidEvidenceLine {
        index: usize,
    },
    CommandVerificationWithoutCommand,
    CompletedWithBlockers,
    CompletedWithoutEvidence,
    CompletedWithFailedVerification,
    NonCompletionWithoutBlocker(HandoffStatus),
}

impl fmt::Display for HandoffValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(detail) => write!(formatter, "malformed structured handoff: {detail}"),
            Self::PayloadTooLarge { actual, maximum } => write!(
                formatter,
                "structured handoff is {actual} bytes, maximum is {maximum}"
            ),
            Self::UnsupportedVersion(version) => {
                write!(
                    formatter,
                    "unsupported structured handoff version {version}"
                )
            }
            Self::StatusMismatch { expected, actual } => write!(
                formatter,
                "handoff status {actual:?} does not match expected status {expected:?}"
            ),
            Self::EmptyField(field) => write!(formatter, "handoff field {field} is empty"),
            Self::FieldTooLarge {
                field,
                actual,
                maximum,
            } => write!(
                formatter,
                "handoff field {field} is {actual} bytes, maximum is {maximum}"
            ),
            Self::TooManyItems {
                field,
                actual,
                maximum,
            } => write!(
                formatter,
                "handoff field {field} has {actual} items, maximum is {maximum}"
            ),
            Self::InvalidPath { field, index, path } => write!(
                formatter,
                "handoff {field}[{index}] path is not normalized workspace-relative: {path:?}"
            ),
            Self::DuplicatePath(path) => {
                write!(formatter, "handoff changed_files repeats path {path:?}")
            }
            Self::DuplicateAttachment(id) => {
                write!(formatter, "handoff attachments repeats artifact {id}")
            }
            Self::InvalidEvidenceLine { index } => {
                write!(
                    formatter,
                    "handoff evidence[{index}] line must be at least 1"
                )
            }
            Self::CommandVerificationWithoutCommand => {
                formatter.write_str("command verification has no command")
            }
            Self::CompletedWithBlockers => {
                formatter.write_str("completed handoff cannot contain blockers")
            }
            Self::CompletedWithoutEvidence => {
                formatter.write_str("completed handoff must contain evidence")
            }
            Self::CompletedWithFailedVerification => {
                formatter.write_str("completed handoff cannot contain failed verification")
            }
            Self::NonCompletionWithoutBlocker(status) => {
                write!(
                    formatter,
                    "{status:?} handoff must explain at least one blocker"
                )
            }
        }
    }
}

impl std::error::Error for HandoffValidationError {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn completed() -> StructuredHandoffV1 {
        StructuredHandoffV1 {
            version: 1,
            status: HandoffStatus::Completed,
            summary: "Implemented the durable contract.".into(),
            evidence: vec![HandoffEvidence {
                path: "ovim-core/src/agent_runtime/handoff.rs".into(),
                line: Some(1),
                claim: "The validator rejects untrusted payloads.".into(),
            }],
            changed_files: vec!["ovim-core/src/agent_runtime/handoff.rs".into()],
            verification: vec![HandoffVerification {
                kind: "command".into(),
                command: Some("cargo test -p ovim-core handoff".into()),
                status: VerificationStatus::Passed,
                detail: None,
            }],
            blockers: vec![],
            followups: vec!["Wire the supervisor in the next slice.".into()],
            attachments: vec![],
            confidence: HandoffConfidence::High,
        }
    }

    #[test]
    fn validates_completed_handoff_and_round_trips_only_as_validated() {
        let validated = HandoffValidator::default()
            .validate(completed(), Some(HandoffStatus::Completed))
            .unwrap();
        let wire = serde_json::to_vec(&validated).unwrap();
        let restored: ValidatedHandoff = serde_json::from_slice(&wire).unwrap();
        assert_eq!(restored, validated);
    }

    #[test]
    fn rejects_malformed_unknown_and_oversized_json() {
        let validator = HandoffValidator::default();
        assert!(matches!(
            validator.validate_json(b"{not-json", None),
            Err(HandoffValidationError::Malformed(_))
        ));

        let unknown = serde_json::to_vec(&json!({
            "version": 1,
            "status": "completed",
            "summary": "done",
            "evidence": [{"path": "src/lib.rs", "claim": "present"}],
            "confidence": "high",
            "surprise": true
        }))
        .unwrap();
        assert!(matches!(
            validator.validate_json(&unknown, None),
            Err(HandoffValidationError::Malformed(_))
        ));

        let bytes = vec![b' '; validator.limits().max_json_bytes + 1];
        assert!(matches!(
            validator.validate_json(&bytes, None),
            Err(HandoffValidationError::PayloadTooLarge { .. })
        ));
    }

    #[test]
    fn rejects_contradictory_status_and_completed_shape() {
        let validator = HandoffValidator::default();
        assert!(matches!(
            validator.validate(completed(), Some(HandoffStatus::Failed)),
            Err(HandoffValidationError::StatusMismatch { .. })
        ));

        let mut missing_evidence = completed();
        missing_evidence.evidence.clear();
        assert_eq!(
            validator.validate(missing_evidence, None).unwrap_err(),
            HandoffValidationError::CompletedWithoutEvidence
        );

        let mut blocked = completed();
        blocked.blockers.push("not actually complete".into());
        assert_eq!(
            validator.validate(blocked, None).unwrap_err(),
            HandoffValidationError::CompletedWithBlockers
        );
    }

    #[test]
    fn validates_unique_attachment_handles() {
        let mut handoff = completed();
        let attachment = ArtifactId::parse("art_durable_report").unwrap();
        handoff.attachments.push(attachment.clone());
        assert!(HandoffValidator::default()
            .validate(handoff.clone(), None)
            .is_ok());
        handoff.attachments.push(attachment.clone());
        assert_eq!(
            HandoffValidator::default()
                .validate(handoff, None)
                .unwrap_err(),
            HandoffValidationError::DuplicateAttachment(attachment)
        );
    }

    #[test]
    fn accepts_partial_non_completion_but_requires_explicit_blocker() {
        let partial = StructuredHandoffV1 {
            version: 1,
            status: HandoffStatus::TimedOut,
            summary: "Timed out after locating the scheduler.".into(),
            evidence: vec![],
            changed_files: vec![],
            verification: vec![],
            blockers: vec!["Child elapsed-time budget was exhausted.".into()],
            followups: vec![],
            attachments: vec![],
            confidence: HandoffConfidence::Medium,
        };
        assert!(HandoffValidator::default()
            .validate(partial.clone(), Some(HandoffStatus::TimedOut))
            .is_ok());

        let mut unexplained = partial;
        unexplained.blockers.clear();
        assert!(matches!(
            HandoffValidator::default().validate(unexplained, None),
            Err(HandoffValidationError::NonCompletionWithoutBlocker(
                HandoffStatus::TimedOut
            ))
        ));
    }

    #[test]
    fn rejects_absolute_traversing_or_non_normalized_paths() {
        for path in [
            "/tmp/result.rs",
            "../src/lib.rs",
            "src/../lib.rs",
            "src//lib.rs",
            "src\\lib.rs",
            "C:/source/lib.rs",
        ] {
            let mut handoff = completed();
            handoff.evidence[0].path = path.into();
            assert!(matches!(
                HandoffValidator::default().validate(handoff, None),
                Err(HandoffValidationError::InvalidPath { .. })
            ));
        }
    }

    #[test]
    fn parent_projection_is_deterministically_bounded_and_unicode_safe() {
        let mut handoff = completed();
        handoff.summary = "\u{00e5}".repeat(2000);
        handoff.evidence = (0..20)
            .map(|index| HandoffEvidence {
                path: format!("src/file_{index}.rs"),
                line: Some(1),
                claim: "evidence ".repeat(100),
            })
            .collect();
        let validated = HandoffValidator::default().validate(handoff, None).unwrap();
        let projection = validated.parent_projection();
        assert!(projection.summary.len() <= 1024);
        assert_eq!(projection.evidence.len(), 8);
        assert_eq!(projection.omitted_evidence, 12);
        assert!(serde_json::to_vec(&projection).unwrap().len() < 16 * 1024);
    }

    #[test]
    fn parent_projection_carries_bounded_system_workspace_warnings() {
        let validated = HandoffValidator::default()
            .validate(completed(), None)
            .unwrap();
        let warnings = (0..20)
            .map(|index| AgentWorkspaceWarning {
                kind: super::super::AgentWorkspaceWarningKind::MissingArtifact,
                path: Some(format!("src/missing-{index}.rs")),
                artifact_id: None,
                detail: "artifact unavailable".into(),
            })
            .collect::<Vec<_>>();

        let projection = validated.parent_projection_with_workspace_warnings(&warnings);
        assert_eq!(projection.workspace_warnings.len(), 16);
        assert_eq!(projection.omitted_workspace_warnings, 4);
        assert!(validated.parent_projection().workspace_warnings.is_empty());
    }
}
