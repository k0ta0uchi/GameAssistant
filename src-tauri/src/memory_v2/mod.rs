//! Phase 2A memory-v2 domain foundation.

pub mod api;
pub mod canonical;
pub mod domain;
pub mod error;
pub mod journal;
pub mod manifest;
pub mod operation;
pub mod paths;
pub mod policy;
pub mod redaction;
pub mod repository;
pub mod subject;
pub mod validation;

#[cfg(test)]
mod tests {
    use super::canonical::canonical_json;
    use super::domain::{
        resolve_fact_conflict, resolve_fact_conflict_checked, EventType, Fact, FactPredicate,
        FactStatus, RawEvent, SourceKind,
    };
    use super::operation::{
        operation_id, OperationDisposition, OperationEnvelope, OperationKind, OperationRegistry,
    };
    use super::paths::MemoryPaths;
    use super::policy::{FactDecision, Policy, PrivacyAdmission, SensitiveCategory};
    use super::redaction::{RedactionCategory, Redactor};
    use super::subject::Subject;
    use super::validation::{validate_embedding, Embedding};
    use super::{journal, manifest};
    use serde_json::json;
    use serde_json::Value;
    use sha2::{Digest, Sha256};
    use std::fs;
    use std::io::Write;
    use std::sync::Arc;
    use std::thread;
    use uuid::Uuid;

    fn temp_root(label: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "gameassistant-memory-v2-{}-{}",
            label,
            Uuid::new_v4()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn test_envelope(kind: OperationKind, entity_id: &str, payload: Value) -> OperationEnvelope {
        OperationEnvelope::new(kind, entity_id, "2025-01-02T03:04:05Z", Some(0), payload).unwrap()
    }

    #[test]
    fn paths_derive_only_from_injected_runtime_root() {
        let fake_root = std::path::PathBuf::from(r"C:\fake\runtime-root");
        let paths = MemoryPaths::from_runtime_root(&fake_root).unwrap();
        assert_eq!(paths.runtime_root(), fake_root.as_path());
        assert_eq!(
            paths.raw_events(),
            fake_root.join("data").join("memory_v2").join("raw_events")
        );
        assert_eq!(
            paths.facts(),
            fake_root.join("data").join("memory_v2").join("facts")
        );
        assert_eq!(
            paths.operations(),
            fake_root.join("data").join("memory_v2").join("operations")
        );
        assert_eq!(
            paths.embeddings(),
            fake_root.join("data").join("memory_v2").join("embeddings")
        );
    }

    #[test]
    fn subjects_parse_and_reject_invalid_ids() {
        assert_eq!(Subject::parse("self").unwrap(), Subject::SelfSubject);
        assert_eq!(
            Subject::parse("twitch:123_abc").unwrap().to_string(),
            "twitch:123_abc"
        );
        let id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        assert_eq!(
            Subject::parse(&format!("manual-person:{id}"))
                .unwrap()
                .to_string(),
            format!("manual-person:{id}")
        );
        assert!(Subject::parse("twitch:").is_err());
        assert!(Subject::parse("twitch:nil").is_err());
        assert!(Subject::parse("discord:bad/id").is_err());
        assert!(Subject::parse("manual-person:not-a-uuid").is_err());
    }

    #[test]
    fn redaction_has_stable_golden_replacements_and_identity() {
        let redactor = Redactor::default();
        let cases = [
            (
                "token=not-a-real-secret",
                "token=[REDACTED:credentials]",
                RedactionCategory::Credentials,
            ),
            (
                "write alice@example.com",
                "write [REDACTED:contact]",
                RedactionCategory::Contact,
            ),
            (
                "card 4111 1111 1111 1111",
                "card [REDACTED:financial]",
                RedactionCategory::Financial,
            ),
            (
                "My number is 123456789012",
                "My number is [REDACTED:government_id]",
                RedactionCategory::GovernmentId,
            ),
            (
                "東京都千代田区1丁目2番3号",
                "[REDACTED:precise_location]",
                RedactionCategory::PreciseLocation,
            ),
            (
                "ship to 123 Main Street",
                "ship to [REDACTED:precise_location]",
                RedactionCategory::PreciseLocation,
            ),
        ];
        for (input, expected, category) in cases {
            let result = redactor.redact(input);
            assert_eq!(result.text(), expected);
            assert!(result.categories().contains(&category));
            assert_eq!(redactor.redact(result.text()).text(), result.text());
        }
    }

    #[test]
    fn policy_rejects_prohibited_categories_from_becoming_facts() {
        let policy = Policy::default();
        let result = Redactor::default().redact("I have diabetes and my token=not-a-real-secret");
        assert_eq!(
            policy.fact_decision(result.text()),
            FactDecision::Prohibited
        );
        assert!(policy.is_prohibited(SensitiveCategory::Health));
        assert!(policy.is_prohibited(SensitiveCategory::Credentials));
    }

    #[test]
    fn japanese_health_redaction_does_not_match_encouragement_prefixes() {
        let redactor = Redactor::default();
        let health = redactor.redact("がんの治療について");
        assert!(health.categories().contains(&RedactionCategory::Health));
        assert!(health.text().contains("[REDACTED:health]"));

        for encouragement in ["がんばる", "がんばってね"] {
            let result = redactor.redact(encouragement);
            assert!(!result.categories().contains(&RedactionCategory::Health));
            assert_eq!(result.text(), encouragement);
        }
    }

    #[test]
    fn canonical_json_and_operation_id_are_order_invariant_and_golden() {
        let first = json!({"b": 2, "a": 1});
        let second = json!({"a": 1, "b": 2});
        assert_eq!(canonical_json(&first).unwrap(), "{\"a\":1,\"b\":2}");
        assert_eq!(
            canonical_json(&json!({"array": [{"z": 0, "a": 1}, {"b": 2}]})).unwrap(),
            "{\"array\":[{\"a\":1,\"z\":0},{\"b\":2}]}"
        );
        assert_eq!(
            operation_id(OperationKind::RawEvent, &first).unwrap(),
            operation_id(OperationKind::RawEvent, &second).unwrap()
        );
        assert_eq!(
            operation_id(OperationKind::RawEvent, &first).unwrap(),
            "2e0162d00a14b5be219ed5fe74fffe64572ee9134ebb5ba11cdb65535de825cb"
        );
    }

    #[test]
    fn raw_events_are_constructible_only_with_validated_redacted_content() {
        let content = Redactor::default().redact_text("hello").unwrap();
        let event = RawEvent::try_new(
            Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap(),
            Subject::SelfSubject,
            "manual",
            content.clone(),
        )
        .unwrap();
        assert_eq!(event.content().as_str(), "hello");
        assert!(RawEvent::try_new(Uuid::nil(), Subject::SelfSubject, "", content).is_err());
    }

    #[test]
    fn fact_status_precedence_and_same_rank_conflicts_are_deterministic() {
        assert!(FactStatus::Auto < FactStatus::Confirmed);
        assert!(FactStatus::Confirmed < FactStatus::Edited);
        let left = Fact::try_derive(
            Subject::SelfSubject,
            "mood",
            "alpha",
            FactStatus::Auto,
            Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap(),
            &Policy::default(),
        )
        .unwrap();
        let right = Fact::try_derive(
            Subject::SelfSubject,
            "mood",
            "beta",
            FactStatus::Edited,
            Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap(),
            &Policy::default(),
        )
        .unwrap();
        assert_eq!(resolve_fact_conflict(&left, &right), right);
        let a = Fact::try_derive(
            Subject::SelfSubject,
            "mood",
            "alpha",
            FactStatus::Confirmed,
            Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap(),
            &Policy::default(),
        )
        .unwrap();
        let b = Fact::try_derive(
            Subject::SelfSubject,
            "mood",
            "beta",
            FactStatus::Confirmed,
            Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap(),
            &Policy::default(),
        )
        .unwrap();
        assert_eq!(resolve_fact_conflict(&a, &b), resolve_fact_conflict(&a, &b));
    }

    #[test]
    fn embeddings_accept_none_or_768_finite_values_only() {
        assert_eq!(validate_embedding(None).unwrap(), None);
        let valid = vec![0.25_f32; 768];
        assert_eq!(
            Embedding::try_from(valid.clone()).unwrap().as_slice(),
            valid.as_slice()
        );
        assert!(Embedding::try_from(vec![0.25_f32; 767]).is_err());
        assert!(Embedding::try_from(vec![f32::NAN; 768]).is_err());
        assert!(Embedding::try_from(vec![f32::INFINITY; 768]).is_err());
    }

    #[test]
    fn raw_event_wire_is_closed_and_requires_canonical_metadata() {
        let event_id = "550e8400-e29b-41d4-a716-446655440000";
        let valid = json!({
            "event_id": event_id,
            "subject": "self",
            "event_type": "human",
            "source": "twitch",
            "occurred_at": "2025-01-02T03:04:05Z",
            "content": "hello"
        });
        let parsed: std::result::Result<RawEvent, _> = serde_json::from_value(valid);
        assert!(parsed.is_ok());

        let unknown = json!({
            "event_id": event_id,
            "subject": "self",
            "event_type": "other",
            "source": "twitch",
            "occurred_at": "2025-01-02T03:04:05Z",
            "content": "hello"
        });
        let parsed: std::result::Result<RawEvent, _> = serde_json::from_value(unknown);
        assert!(parsed.is_err());

        let noncanonical_time = json!({
            "event_id": event_id,
            "subject": "self",
            "event_type": "human",
            "source": "twitch",
            "occurred_at": "2025-01-02T12:04:05+09:00",
            "content": "hello"
        });
        let parsed: std::result::Result<RawEvent, _> = serde_json::from_value(noncanonical_time);
        assert!(parsed.is_err());
    }

    #[test]
    fn raw_event_rejects_raw_source_and_noncanonical_event_id() {
        let base = json!({
            "event_id": "550e8400-e29b-41d4-a716-446655440000",
            "subject": "self",
            "event_type": "human",
            "source": "twitch",
            "occurred_at": "2025-01-02T03:04:05Z",
            "content": "hello"
        });
        let mut raw_source = base.clone();
        raw_source["source"] = json!("alice@example.com");
        let parsed: std::result::Result<RawEvent, _> = serde_json::from_value(raw_source);
        assert!(parsed.is_err());

        let mut uppercase_id = base;
        uppercase_id["event_id"] = json!("550E8400-E29B-41D4-A716-446655440000");
        let parsed: std::result::Result<RawEvent, _> = serde_json::from_value(uppercase_id);
        assert!(parsed.is_err());
    }

    #[test]
    fn source_kind_is_a_closed_canonical_wire_allowlist() {
        for (kind, wire) in [
            (SourceKind::Microphone, "microphone"),
            (SourceKind::Discord, "discord"),
            (SourceKind::Twitch, "twitch"),
            (SourceKind::Manual, "manual"),
            (SourceKind::System, "system"),
        ] {
            assert_eq!(serde_json::to_string(&kind).unwrap(), format!("\"{wire}\""));
            assert_eq!(
                serde_json::from_str::<SourceKind>(&format!("\"{wire}\"")).unwrap(),
                kind
            );
        }
        for wire in ["", "nil", "ai", "human", "auto", "unknown"] {
            assert!(serde_json::from_str::<SourceKind>(&format!("\"{wire}\"")).is_err());
        }
    }

    #[test]
    fn identity_and_operation_fields_reject_nil_or_empty_values() {
        let valid_event = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let content = Redactor::default().redact_text("hello").unwrap();
        assert!(
            RawEvent::try_new(Uuid::nil(), Subject::SelfSubject, "manual", content.clone())
                .is_err()
        );
        assert!(Fact::try_derive(
            Subject::SelfSubject,
            "display_name",
            "Ada",
            FactStatus::Auto,
            Uuid::nil(),
            &Policy::default(),
        )
        .is_err());
        assert!(Fact::try_derive_with_metadata(
            Subject::SelfSubject,
            FactPredicate::Fact,
            "display_name",
            "Ada",
            FactStatus::Auto,
            valid_event,
            0,
            "",
            &Policy::default(),
        )
        .is_err());
        let missing_fact_id = json!({
            "subject": "self", "predicate": "fact", "key": "name", "value": "Ada",
            "status": "auto", "source_event_id": valid_event.to_string(), "revision": 0,
            "operation_id": "op-1"
        });
        assert!(serde_json::from_value::<Fact>(missing_fact_id).is_err());
    }

    #[test]
    fn transition_cas_requires_revision_and_status() {
        let source_event_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let current = Fact::try_derive_with_metadata(
            Subject::SelfSubject,
            FactPredicate::Fact,
            "name",
            "Ada",
            FactStatus::Auto,
            source_event_id,
            4,
            "op-a",
            &Policy::default(),
        )
        .unwrap();
        let proposed = Fact::try_derive_with_metadata(
            Subject::SelfSubject,
            FactPredicate::Fact,
            "name",
            "Ada",
            FactStatus::Confirmed,
            source_event_id,
            4,
            "op-b",
            &Policy::default(),
        )
        .unwrap();
        assert!(current.apply_transition(&proposed, Some(4), None).is_err());
        assert!(current
            .apply_transition(&proposed, Some(4), Some(FactStatus::Confirmed))
            .is_err());
        assert!(current
            .apply_transition(&proposed, Some(3), Some(FactStatus::Auto))
            .is_err());
        assert!(current
            .apply_transition(&proposed, Some(4), Some(FactStatus::Auto))
            .is_ok());
    }

    #[test]
    fn equal_status_conflicts_are_argument_order_independent_through_payload_bytes() {
        let source_event_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let left = Fact::try_derive_with_metadata(
            Subject::SelfSubject,
            FactPredicate::Fact,
            "name",
            "Ada",
            FactStatus::Confirmed,
            source_event_id,
            7,
            "op-z",
            &Policy::default(),
        )
        .unwrap();
        let right = Fact::try_derive_with_metadata(
            Subject::SelfSubject,
            FactPredicate::Fact,
            "name",
            "Bea",
            FactStatus::Confirmed,
            source_event_id,
            7,
            "op-z",
            &Policy::default(),
        )
        .unwrap();
        let winner = resolve_fact_conflict(&left, &right);
        assert_eq!(winner, resolve_fact_conflict(&right, &left));
        assert_eq!(winner.value(), "Ada");
    }

    #[test]
    fn fact_wire_contains_stable_identity_revision_and_controlled_predicate() {
        let fact = Fact::try_derive(
            Subject::SelfSubject,
            "display_name",
            "Ada",
            FactStatus::Auto,
            Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap(),
            &Policy::default(),
        )
        .unwrap();
        let wire = serde_json::to_value(&fact).unwrap();
        assert!(wire.get("fact_id").and_then(Value::as_str).is_some());
        assert_eq!(wire.get("revision").and_then(Value::as_u64), Some(0));
        assert!(wire.get("predicate").and_then(Value::as_str).is_some());
        assert_eq!(wire["fact_id"], serde_json::json!("fact:self:display_name"));
    }

    #[test]
    fn facts_allow_only_monotonic_status_transitions_with_revision_cas() {
        let source_event_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let current = Fact::try_derive_with_metadata(
            Subject::SelfSubject,
            FactPredicate::Profile,
            "display_name",
            "Ada",
            FactStatus::Auto,
            source_event_id,
            4,
            "op-a",
            &Policy::default(),
        )
        .unwrap();
        let confirmed = Fact::try_derive_with_metadata(
            Subject::SelfSubject,
            FactPredicate::Profile,
            "display_name",
            "Ada",
            FactStatus::Confirmed,
            source_event_id,
            4,
            "op-b",
            &Policy::default(),
        )
        .unwrap();
        let next = current
            .apply_transition(&confirmed, Some(4), Some(FactStatus::Auto))
            .unwrap();
        assert_eq!(next.status(), FactStatus::Confirmed);
        assert_eq!(next.revision(), 5);
        assert!(current
            .apply_transition(&confirmed, Some(3), Some(FactStatus::Auto))
            .is_err());

        let auto_again = Fact::try_derive(
            Subject::SelfSubject,
            "display_name",
            "Ada",
            FactStatus::Auto,
            source_event_id,
            &Policy::default(),
        )
        .unwrap();
        assert!(next
            .apply_transition(&auto_again, Some(5), Some(FactStatus::Confirmed))
            .is_err());
        assert!(resolve_fact_conflict_checked(&current, &auto_again).is_ok());
        assert!(resolve_fact_conflict_checked(
            &current,
            &Fact::try_derive(
                Subject::SelfSubject,
                "other",
                "Ada",
                FactStatus::Edited,
                source_event_id,
                &Policy::default(),
            )
            .unwrap()
        )
        .is_err());
    }

    #[test]
    fn operation_envelopes_verify_ids_and_retries_are_byte_no_ops() {
        let payload = json!({"key": "display_name", "value": "Ada"});
        let envelope = OperationEnvelope::new(
            OperationKind::Fact,
            "fact:self:display_name",
            "2025-01-02T03:04:05Z",
            Some(4),
            payload,
        )
        .unwrap();
        assert_eq!(
            envelope.schema_version(),
            super::operation::OPERATION_SCHEMA_VERSION
        );
        assert_eq!(
            envelope.operation_id(),
            "33e9988e832012c4f5456965a515301854a9a8ade1228b97f9c07572246a5de3"
        );
        let bytes = envelope.canonical_bytes().unwrap();
        let decoded: super::operation::OperationEnvelope = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded.canonical_bytes().unwrap(), bytes);
        let mut registry = OperationRegistry::default();
        assert_eq!(
            registry.admit(&envelope).unwrap(),
            OperationDisposition::Accepted
        );
        assert_eq!(
            registry.admit(&decoded).unwrap(),
            OperationDisposition::NoOpRetry
        );
        let later = OperationEnvelope::new(
            OperationKind::Fact,
            "fact:self:display_name",
            "2025-01-02T03:04:06Z",
            Some(4),
            json!({"value": "Ada", "key": "display_name"}),
        )
        .unwrap();
        assert_ne!(envelope.operation_id(), later.operation_id());
        assert_eq!(
            String::from_utf8(bytes.clone()).unwrap(),
            canonical_json(&serde_json::to_value(&envelope).unwrap()).unwrap()
        );
        assert!(serde_json::to_value(&envelope)
            .unwrap()
            .get("operation_kind")
            .is_some());
        assert!(serde_json::to_value(&envelope)
            .unwrap()
            .get("kind")
            .is_none());
    }

    #[test]
    fn privacy_admission_requires_consent_for_private_content_and_fails_closed() {
        let policy = Policy::default();
        let event_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        assert!(policy
            .admit_raw_text(
                "hello",
                PrivacyAdmission {
                    private: true,
                    consent: false
                }
            )
            .is_err());
        assert!(RawEvent::try_new_admitted(
            event_id,
            Subject::SelfSubject,
            EventType::Human,
            "twitch",
            "2025-01-02T03:04:05Z",
            "hello",
            &policy,
            PrivacyAdmission {
                private: true,
                consent: false,
            },
        )
        .is_err());
        assert!(RawEvent::try_new_admitted(
            event_id,
            Subject::SelfSubject,
            EventType::Human,
            "twitch",
            "2025-01-02T03:04:05Z",
            "hello",
            &policy,
            PrivacyAdmission::private_with_consent(),
        )
        .is_ok());
        let admitted = policy
            .admit_raw_text(
                "contact alice@example.com",
                PrivacyAdmission::private_with_consent(),
            )
            .unwrap();
        assert_eq!(admitted.as_str(), "contact [REDACTED:contact]");
        assert!(policy
            .admit_fact_text("my token=not-a-real-secret")
            .is_err());
    }

    #[test]
    fn raw_event_serialization_exposes_only_the_fixed_redacted_wire_fields() {
        let event = RawEvent::try_new_with_metadata(
            Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap(),
            Subject::SelfSubject,
            EventType::Human,
            "twitch",
            "2025-01-02T03:04:05Z",
            Redactor::default().redact_text("hello").unwrap(),
        )
        .unwrap();
        let wire = serde_json::to_value(event).unwrap();
        assert_eq!(wire.as_object().unwrap().len(), 6);
        assert_eq!(wire["event_type"], "human");
        assert_eq!(wire["occurred_at"], "2025-01-02T03:04:05Z");
        assert!(wire.get("raw_text").is_none());
    }

    #[test]
    fn paths_expose_isolated_journal_manifest_lock_staging_and_legacy_locations() {
        let root = temp_root("paths");
        let paths = MemoryPaths::from_runtime_root(&root).unwrap();
        assert_eq!(paths.journal(), root.join("data/memory_v2/journal.jsonl"));
        assert_eq!(paths.manifest(), root.join("data/memory_v2/manifest.json"));
        assert_eq!(paths.lock(), root.join("data/memory_v2/journal.lock"));
        assert_eq!(paths.staging(), root.join("data/memory_v2/staging"));
        assert_eq!(paths.legacy(), root.join("data/memory_v2/legacy"));
    }

    #[test]
    fn paths_reject_relative_forbidden_and_traversing_roots() {
        assert!(MemoryPaths::from_runtime_root("relative").is_err());
        // The root is explicitly injected by the caller.  A current-directory
        // path is therefore valid; the production resolver simply never
        // falls back to it implicitly.
        assert!(MemoryPaths::from_runtime_root(std::env::current_dir().unwrap()).is_ok());
        assert!(
            MemoryPaths::from_runtime_root(std::path::PathBuf::from(r"C:\absolute\..\escape"))
                .is_err()
        );
    }

    #[test]
    fn journal_appends_canonical_checksummed_intent_and_commit_durably() {
        let root = temp_root("journal");
        let paths = MemoryPaths::from_runtime_root(&root).unwrap();
        let journal = journal::Journal::open_with_lock(paths.journal(), paths.lock()).unwrap();
        let envelope = test_envelope(
            OperationKind::RawEvent,
            "event-1",
            json!({"z": 2, "a": {"b": true, "a": 1}}),
        );
        journal
            .write_batch(
                "batch-1",
                &[(
                    envelope.operation_id().to_string(),
                    envelope.kind(),
                    serde_json::to_value(&envelope).unwrap(),
                )],
            )
            .unwrap();
        let text = fs::read_to_string(paths.journal()).unwrap();
        assert!(text.ends_with('\n'));
        assert!(text.contains("\"payload\":{\"a\":{\"a\":1,\"b\":true},\"z\":2}"));
        let report = journal.recover().unwrap();
        assert_eq!(report.records().len(), 3);
        assert_eq!(
            report.classifications()[0].classification,
            journal::RecoveryClassification::Committed
        );
    }

    #[test]
    fn journal_ignores_only_an_incomplete_final_line_but_rejects_malformed_complete_lines() {
        let root = temp_root("tails");
        let paths = MemoryPaths::from_runtime_root(&root).unwrap();
        let journal = journal::Journal::open_with_lock(paths.journal(), paths.lock()).unwrap();
        journal
            .write_batch(
                "batch-1",
                &[{
                    let envelope =
                        test_envelope(OperationKind::RawEvent, "event-1", json!({"a": 1}));
                    (
                        envelope.operation_id().to_string(),
                        envelope.kind(),
                        serde_json::to_value(envelope).unwrap(),
                    )
                }],
            )
            .unwrap();
        let valid_records = journal.recover().unwrap().records().len();
        fs::OpenOptions::new()
            .append(true)
            .open(paths.journal())
            .unwrap()
            .write_all(b"{\"schema\":\"broken\"")
            .unwrap();
        let report = journal.recover().unwrap();
        assert!(report.incomplete_final_line());
        assert_eq!(report.records().len(), valid_records);
        assert_eq!(report.replayable_records().len(), 1);
        fs::OpenOptions::new()
            .append(true)
            .open(paths.journal())
            .unwrap()
            .write_all(b"\nnot-json\n")
            .unwrap();
        assert!(matches!(
            journal.recover(),
            Err(journal::JournalError::MalformedCompleteLine { .. })
        ));
    }

    #[test]
    fn journal_rejects_duplicate_conflicts_sequence_checksum_and_unknown_state() {
        let root = temp_root("validation");
        let paths = MemoryPaths::from_runtime_root(&root).unwrap();
        let journal = journal::Journal::open_with_lock(paths.journal(), paths.lock()).unwrap();
        let first = test_envelope(OperationKind::RawEvent, "event-1", json!({"a": 1}));
        let conflicting = test_envelope(OperationKind::RawEvent, "event-1", json!({"a": 2}));
        journal.begin_batch("batch-a").unwrap();
        journal
            .append_operation_envelope("batch-a", &first)
            .unwrap();
        assert!(matches!(
            journal.append_operation(
                "batch-a",
                first.operation_id(),
                conflicting.kind(),
                serde_json::to_value(&conflicting).unwrap(),
            ),
            Err(journal::JournalError::ConflictingOperation { .. })
        ));
        journal.commit_batch("batch-a").unwrap();
        journal.begin_batch("batch-b").unwrap();
        assert!(matches!(
            journal.append_operation_envelope("batch-b", &first),
            Err(journal::JournalError::DuplicateOperation { .. })
        ));
        let mut text = fs::read_to_string(paths.journal()).unwrap();
        let original = text.clone();
        text = text.replace("\"checksum\":\"", "\"checksum\":\"0");
        fs::write(paths.journal(), text).unwrap();
        assert!(matches!(
            journal.recover(),
            Err(journal::JournalError::ChecksumMismatch { .. })
        ));
        text = original;
        // The journal is JSONL; mutate one complete frame rather than trying
        // to deserialize the whole multi-line file as a single JSON value.
        let mut record: serde_json::Value =
            serde_json::from_str(text.lines().next().unwrap()).unwrap();
        record["sequence"] = json!(2);
        record.as_object_mut().unwrap().remove("checksum");
        let unsigned = canonical_json(&record).unwrap();
        let mut hasher = Sha256::new();
        hasher.update(unsigned.as_bytes());
        record["checksum"] = json!(format!("{:x}", hasher.finalize()));
        fs::write(
            paths.journal(),
            format!("{}\n", canonical_json(&record).unwrap()),
        )
        .unwrap();
        assert!(matches!(
            journal.recover(),
            Err(journal::JournalError::SequenceMismatch { .. })
        ));
        fs::write(paths.journal(), "{\"state\":\"mystery\"}\n").unwrap();
        assert!(matches!(
            journal.recover(),
            Err(journal::JournalError::UnknownState { .. })
        ));
    }

    #[test]
    fn committed_retries_are_deterministic_no_ops() {
        let root = temp_root("retry");
        let paths = MemoryPaths::from_runtime_root(&root).unwrap();
        let journal = journal::Journal::open_with_lock(paths.journal(), paths.lock()).unwrap();
        let envelope = test_envelope(OperationKind::RawEvent, "event-1", json!({"a": 1}));
        let payload = serde_json::to_value(&envelope).unwrap();
        journal.begin_batch("batch-1").unwrap();
        assert!(matches!(
            journal.append_operation_envelope("batch-1", &envelope),
            Ok(journal::AppendOutcome::Appended { .. })
        ));
        assert!(matches!(
            journal.commit_batch("batch-1"),
            Ok(journal::AppendOutcome::Appended { .. })
        ));
        assert!(matches!(
            journal.begin_batch("batch-1"),
            Ok(journal::AppendOutcome::NoOpRetry { .. })
        ));
        assert!(matches!(
            journal.write_batch(
                "batch-1",
                &[(envelope.operation_id().to_string(), envelope.kind(), payload)]
            ),
            Ok(outcomes) if outcomes == vec![journal::AppendOutcome::NoOpRetry { sequence: 3 }]
        ));
        assert_eq!(
            journal.recover().unwrap().classifications()[0].classification,
            journal::RecoveryClassification::Committed
        );
    }

    #[test]
    fn concurrent_writers_have_unique_monotonic_sequences() {
        let root = temp_root("concurrent");
        let paths = MemoryPaths::from_runtime_root(&root).unwrap();
        let journal =
            Arc::new(journal::Journal::open_with_lock(paths.journal(), paths.lock()).unwrap());
        let mut workers = Vec::new();
        for index in 0..8 {
            let journal = Arc::clone(&journal);
            workers.push(thread::spawn(move || {
                let envelope = test_envelope(
                    OperationKind::RawEvent,
                    &format!("event-{index}"),
                    json!({"index": index}),
                );
                journal
                    .write_batch(
                        &format!("batch-{index}"),
                        &[(
                            envelope.operation_id().to_string(),
                            envelope.kind(),
                            serde_json::to_value(envelope).unwrap(),
                        )],
                    )
                    .unwrap();
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }
        let report = journal.recover().unwrap();
        assert_eq!(report.records().len(), 24);
        assert_eq!(report.replayable_records().len(), 8);
        assert!(report
            .records()
            .windows(2)
            .all(|pair| pair[0].sequence() + 1 == pair[1].sequence()));
    }

    #[test]
    fn manifest_selector_validates_current_store_names_and_monotonic_generation() {
        let root = temp_root("manifest");
        let paths = MemoryPaths::from_runtime_root(&root).unwrap();
        let selector = manifest::ManifestSelector::new(paths.manifest(), paths.staging()).unwrap();
        fs::write(paths.journal(), b"journal\n").unwrap();
        for name in [
            manifest::RAW_EVENTS_TABLE,
            manifest::FACTS_TABLE,
            manifest::EMBEDDINGS_TABLE,
        ] {
            fs::create_dir_all(root.join("data/memory_v2").join(name)).unwrap();
        }
        let manifest = selector.select(7, 42).unwrap();
        assert_eq!(manifest.raw_events(), manifest::RAW_EVENTS_TABLE);
        assert_eq!(manifest.facts(), manifest::FACTS_TABLE);
        assert_eq!(manifest.embeddings(), manifest::EMBEDDINGS_TABLE);
        assert_eq!(selector.read().unwrap(), Some(manifest));
        assert!(matches!(
            selector.select(7, 43),
            Err(manifest::ManifestError::GenerationNotMonotonic { .. })
        ));
        let next = selector.select(8, 43).unwrap();
        assert_eq!(next.generation(), 8);
        let mut invalid = selector.read().unwrap().unwrap();
        invalid.set_events("../escape".to_string());
        assert!(selector.replace(&invalid).is_err());
    }
}
