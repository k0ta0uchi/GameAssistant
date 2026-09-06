use super::error::{MemoryError, Result};
use super::redaction::{RedactedText, Redactor};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SensitiveCategory {
    Credentials,
    Contact,
    Financial,
    GovernmentId,
    Health,
    PreciseLocation,
    Other(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FactDecision {
    Allowed,
    Prohibited,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrivacyAdmission {
    pub private: bool,
    pub consent: bool,
}

impl PrivacyAdmission {
    pub const fn public() -> Self {
        Self {
            private: false,
            consent: false,
        }
    }
    pub const fn private_with_consent() -> Self {
        Self {
            private: true,
            consent: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    prohibited: BTreeSet<SensitiveCategory>,
}

impl Default for Policy {
    fn default() -> Self {
        Self::with_prohibited_categories([
            SensitiveCategory::Credentials,
            SensitiveCategory::Contact,
            SensitiveCategory::Financial,
            SensitiveCategory::GovernmentId,
            SensitiveCategory::Health,
            SensitiveCategory::PreciseLocation,
        ])
    }
}

impl Policy {
    pub fn with_prohibited_categories<I>(categories: I) -> Self
    where
        I: IntoIterator<Item = SensitiveCategory>,
    {
        Self {
            prohibited: categories.into_iter().collect(),
        }
    }
    pub fn is_prohibited(&self, category: SensitiveCategory) -> bool {
        self.prohibited.contains(&category)
    }
    pub fn prohibited_categories(&self) -> &BTreeSet<SensitiveCategory> {
        &self.prohibited
    }

    pub fn fact_decision(&self, redacted_text: &str) -> FactDecision {
        let markers = [
            ("[REDACTED:credentials]", SensitiveCategory::Credentials),
            ("[REDACTED:contact]", SensitiveCategory::Contact),
            ("[REDACTED:financial]", SensitiveCategory::Financial),
            ("[REDACTED:government_id]", SensitiveCategory::GovernmentId),
            ("[REDACTED:health]", SensitiveCategory::Health),
            (
                "[REDACTED:precise_location]",
                SensitiveCategory::PreciseLocation,
            ),
        ];
        if markers.iter().any(|(marker, category)| {
            redacted_text.contains(marker) && self.is_prohibited(category.clone())
        }) {
            FactDecision::Prohibited
        } else {
            FactDecision::Allowed
        }
    }

    pub fn admit_raw_text(&self, input: &str, admission: PrivacyAdmission) -> Result<RedactedText> {
        // A private admission without explicit consent is never allowed to
        // fall through to redaction or persistence.
        if admission.private && !admission.consent {
            return Err(MemoryError::ProhibitedCategory(
                "private memory requires consent".to_string(),
            ));
        }
        let redacted = Redactor::default()
            .redact_text(input)
            .map_err(|_| MemoryError::InvalidContent)?;
        if redacted.as_str().trim().is_empty()
            || Redactor::default().redact(redacted.as_str()).text() != redacted.as_str()
        {
            return Err(MemoryError::InvalidContent);
        }
        Ok(redacted)
    }

    pub fn admit_fact_text(&self, input: &str) -> Result<RedactedText> {
        let redacted = self.admit_raw_text(input, PrivacyAdmission::public())?;
        if self.fact_decision(redacted.as_str()) == FactDecision::Prohibited {
            return Err(MemoryError::ProhibitedCategory(
                "configured sensitive category".to_string(),
            ));
        }
        Ok(redacted)
    }
}
