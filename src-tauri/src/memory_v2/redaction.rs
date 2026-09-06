use super::error::{MemoryError, Result};
use regex::Regex;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::ops::Deref;
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RedactionCategory {
    Credentials,
    Contact,
    Financial,
    GovernmentId,
    PreciseLocation,
    Health,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedactedText(String);

impl RedactedText {
    pub(crate) fn from_validated(value: String) -> Self {
        Self(value)
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Deref for RedactedText {
    type Target = str;
    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl Serialize for RedactedText {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RedactedText {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if value.trim().is_empty() || Redactor::default().redact(&value).text() != value {
            return Err(serde::de::Error::custom(MemoryError::InvalidContent));
        }
        Ok(Self::from_validated(value))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedactionResult {
    text: RedactedText,
    categories: Vec<RedactionCategory>,
}

impl RedactionResult {
    pub fn text(&self) -> &str {
        self.text.as_str()
    }
    pub fn redacted_text(&self) -> &RedactedText {
        &self.text
    }
    pub fn into_redacted_text(self) -> RedactedText {
        self.text
    }
    pub fn categories(&self) -> &[RedactionCategory] {
        &self.categories
    }
    pub fn is_prohibited(&self) -> bool {
        !self.categories.is_empty()
    }
}

#[derive(Clone, Debug, Default)]
pub struct Redactor;

impl Redactor {
    pub fn redact(&self, input: &str) -> RedactionResult {
        let mut text = input.to_string();
        let mut categories = Vec::new();
        replace_regex(
            &mut text,
            credentials_regex(),
            "$1[REDACTED:credentials]",
            RedactionCategory::Credentials,
            &mut categories,
        );
        replace_regex(
            &mut text,
            bearer_regex(),
            "[REDACTED:credentials]",
            RedactionCategory::Credentials,
            &mut categories,
        );
        replace_regex(
            &mut text,
            token_prefix_regex(),
            "[REDACTED:credentials]",
            RedactionCategory::Credentials,
            &mut categories,
        );
        replace_cards(&mut text, &mut categories);
        replace_regex(
            &mut text,
            government_id_regex(),
            "[REDACTED:government_id]",
            RedactionCategory::GovernmentId,
            &mut categories,
        );
        replace_regex(
            &mut text,
            precise_location_regex(),
            "[REDACTED:precise_location]",
            RedactionCategory::PreciseLocation,
            &mut categories,
        );
        replace_regex(
            &mut text,
            health_regex(),
            "[REDACTED:health]$1",
            RedactionCategory::Health,
            &mut categories,
        );
        replace_regex(
            &mut text,
            contact_regex(),
            "[REDACTED:contact]",
            RedactionCategory::Contact,
            &mut categories,
        );
        RedactionResult {
            text: RedactedText::from_validated(text),
            categories,
        }
    }

    pub fn redact_text(&self, input: &str) -> Result<RedactedText> {
        if input.trim().is_empty() {
            return Err(MemoryError::InvalidContent);
        }
        let result = self.redact(input);
        Ok(result.text)
    }
}

fn replace_regex(
    text: &mut String,
    regex: &Regex,
    replacement: &str,
    category: RedactionCategory,
    categories: &mut Vec<RedactionCategory>,
) {
    if regex.is_match(text) {
        *text = regex.replace_all(text, replacement).into_owned();
        if !categories.contains(&category) {
            categories.push(category);
        }
    }
}

fn replace_cards(text: &mut String, categories: &mut Vec<RedactionCategory>) {
    static CARD: OnceLock<Regex> = OnceLock::new();
    let regex = CARD.get_or_init(|| Regex::new(r"(?:[0-9][ -]?){13,19}").expect("card regex"));
    let mut output = String::with_capacity(text.len());
    let mut cursor = 0;
    let mut found = false;
    for matched in regex.find_iter(text) {
        let candidate = matched.as_str();
        let digits: String = candidate.chars().filter(|ch| ch.is_ascii_digit()).collect();
        if (13..=19).contains(&digits.len()) && luhn(&digits) {
            output.push_str(&text[cursor..matched.start()]);
            output.push_str("[REDACTED:financial]");
            cursor = matched.end();
            found = true;
        }
    }
    if found {
        output.push_str(&text[cursor..]);
        *text = output;
        if !categories.contains(&RedactionCategory::Financial) {
            categories.push(RedactionCategory::Financial);
        }
    }
}

fn luhn(digits: &str) -> bool {
    let mut sum = 0;
    let mut double = false;
    for digit in digits.bytes().rev() {
        let mut value = (digit - b'0') as u32;
        if double {
            value *= 2;
            if value > 9 {
                value -= 9;
            }
        }
        sum += value;
        double = !double;
    }
    sum % 10 == 0
}

fn credentials_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"(?i)(\b(?:password|passwd|passphrase|token|api[_ -]?key|secret|authorization)\s*[:=]\s*)\S+").expect("credentials regex"))
}
fn bearer_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"(?i)\bBearer\s+[A-Za-z0-9._~+/=-]+").expect("bearer regex"))
}
fn token_prefix_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"\b(?:sk-[A-Za-z0-9_-]{16,}|gh[pousr]_[A-Za-z0-9_]{20,}|AKIA[0-9A-Z]{16})\b")
            .expect("token prefix regex")
    })
}
fn government_id_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"\b\d{3}-\d{2}-\d{4}\b|\b\d{12}\b").expect("government id regex")
    })
}
fn precise_location_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"〒?[0-9]{3}-[0-9]{4}|(?:北海道|東京都|(?:京都|大阪)府|[一-龠ぁ-んァ-ヶ]{2,3}県)[^\n]{0,30}?[0-9０-９]+丁目[0-9０-９]+番(?:地)?[0-9０-９]+号|\b[0-9]{1,5}\s+[A-Za-z0-9][A-Za-z0-9 .'-]{1,40}\s+(?:Street|St|Road|Rd|Avenue|Ave|Boulevard|Blvd|Lane|Ln|Drive|Dr)\b").expect("address regex"))
}
fn health_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            // `regex` has no look-around support.  Capture the character after
            // the Japanese cancer term so it can be restored while excluding
            // the encouragement stem `がんば...`.
            r"(?i)\b(?:diabetes|cancer|depression|medication)\b|糖尿病|がん([^ば]|$)|鬱病|うつ病|薬物治療",
        )
        .expect("health regex")
    })
}
fn contact_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"(?i)\b[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}\b|(?:\+?\d[\d ()-]{7,}\d)")
            .expect("contact regex")
    })
}

pub fn redact(input: &str) -> RedactionResult {
    Redactor::default().redact(input)
}
