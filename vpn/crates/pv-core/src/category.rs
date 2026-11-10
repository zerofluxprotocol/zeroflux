//! Built-in concept catalog.

use crate::types::Sensitivity;
use schemars::JsonSchema;
use serde::Serialize;

pub const EMAIL: &str = "EMAIL";
pub const PHONE: &str = "PHONE";
pub const PERSON: &str = "PERSON";
pub const ADDRESS: &str = "ADDRESS";
pub const GOVERNMENT_ID: &str = "GOVERNMENT_ID";
pub const PASSPORT: &str = "PASSPORT";
pub const DRIVER_LICENSE: &str = "DRIVER_LICENSE";
pub const DOB: &str = "DOB";
pub const FINANCIAL_ACCOUNT: &str = "FINANCIAL_ACCOUNT";
pub const PAYMENT_CARD: &str = "PAYMENT_CARD";
pub const HEALTH_INFORMATION: &str = "HEALTH_INFORMATION";
pub const AUTH_SECRET: &str = "AUTH_SECRET";
pub const API_KEY: &str = "API_KEY";
pub const PRIVATE_KEY: &str = "PRIVATE_KEY";
pub const PUBLIC_INFORMATION: &str = "PUBLIC_INFORMATION";

/// Registered exact values.
pub const EXACT_SECRET: &str = "EXACT_SECRET";
/// Prefix for owner-defined entity types found by the span model: `CUSTOM:<entity_id>`.
pub const CUSTOM_PREFIX: &str = "CUSTOM:";
/// Sensitive content was indicated but could not be localized or confidently typed.
pub const UNCERTAIN: &str = "UNCERTAIN";
/// A required classifier did not complete.
pub const CLASSIFIER_FAILURE: &str = "CLASSIFIER_FAILURE";
/// Content the proxy cannot inspect (e.g. images) under a protective policy.
pub const UNSUPPORTED_CONTENT: &str = "UNSUPPORTED_CONTENT";

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CategoryInfo {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub sensitivity: Sensitivity,
    /// Critical categories are blocked for remote destinations by default.
    pub critical: bool,
    pub examples: &'static [&'static str],
}

pub const BUILTIN: &[CategoryInfo] = &[
    CategoryInfo { id: GOVERNMENT_ID, label: "SSN and government IDs", description: "Social Security numbers, tax IDs and similar national identifiers", sensitivity: Sensitivity::Critical, critical: true, examples: &["123-45-6789 (synthetic)"] },
    CategoryInfo { id: PASSPORT, label: "Passport numbers", description: "Passport numbers near a passport cue", sensitivity: Sensitivity::Critical, critical: true, examples: &["Passport no. X1234567"] },
    CategoryInfo { id: DRIVER_LICENSE, label: "Driver's licenses", description: "Driver's license numbers near a license cue", sensitivity: Sensitivity::Critical, critical: true, examples: &["DL# D1234567"] },
    CategoryInfo { id: PAYMENT_CARD, label: "Payment cards", description: "Credit and debit card numbers (Luhn-checked)", sensitivity: Sensitivity::Critical, critical: true, examples: &["4111 1111 1111 1111"] },
    CategoryInfo { id: API_KEY, label: "API keys", description: "Provider API keys and access tokens", sensitivity: Sensitivity::Critical, critical: true, examples: &["sk-...", "AKIA...", "ghp_..."] },
    CategoryInfo { id: PRIVATE_KEY, label: "Private keys", description: "PEM/OpenSSH private keys and raw wallet keys", sensitivity: Sensitivity::Critical, critical: true, examples: &["-----BEGIN PRIVATE KEY-----"] },
    CategoryInfo { id: AUTH_SECRET, label: "Passwords and auth secrets", description: "Passwords, bearer tokens, JWTs, credentials in URLs", sensitivity: Sensitivity::Critical, critical: true, examples: &["password=hunter2", "Bearer eyJ..."] },
    CategoryInfo { id: FINANCIAL_ACCOUNT, label: "Bank accounts", description: "IBANs, routing and account numbers", sensitivity: Sensitivity::High, critical: false, examples: &["GB82 WEST 1234 5698 7654 32"] },
    CategoryInfo { id: HEALTH_INFORMATION, label: "Health information", description: "Diagnoses, conditions, medications tied to a person", sensitivity: Sensitivity::High, critical: false, examples: &["diagnosed with type 2 diabetes"] },
    CategoryInfo { id: DOB, label: "Dates of birth", description: "Dates near a birth-date cue", sensitivity: Sensitivity::High, critical: false, examples: &["DOB: 1984-03-12"] },
    CategoryInfo { id: EMAIL, label: "Email", description: "Email addresses and email-like identifiers", sensitivity: Sensitivity::Moderate, critical: false, examples: &["jane@example.com"] },
    CategoryInfo { id: PHONE, label: "Phone numbers", description: "Phone numbers in common national and E.164 formats", sensitivity: Sensitivity::Moderate, critical: false, examples: &["(555) 010-1234"] },
    CategoryInfo { id: ADDRESS, label: "Postal addresses", description: "Street addresses", sensitivity: Sensitivity::Moderate, critical: false, examples: &["221 Baker Street"] },
    CategoryInfo { id: PERSON, label: "Names", description: "Personal names (full names, user or account names)", sensitivity: Sensitivity::Moderate, critical: false, examples: &["Jane Doe"] },
    CategoryInfo { id: PUBLIC_INFORMATION, label: "Public information", description: "Information marked as public; allowed by default", sensitivity: Sensitivity::Low, critical: false, examples: &["company press release"] },
];

pub fn info(id: &str) -> Option<&'static CategoryInfo> {
    BUILTIN.iter().find(|c| c.id == id)
}

pub fn is_critical(id: &str) -> bool {
    info(id).map(|c| c.critical).unwrap_or(false)
}

pub fn sensitivity_of(id: &str) -> Sensitivity {
    if let Some(i) = info(id) {
        return i.sensitivity;
    }
    if id == EXACT_SECRET || id.starts_with("EXACT_SECRET:") {
        return Sensitivity::Critical;
    }
    if id.starts_with(CUSTOM_PREFIX) || id == UNCERTAIN || id == CLASSIFIER_FAILURE {
        return Sensitivity::High;
    }
    Sensitivity::Moderate
}

pub fn critical_ids() -> Vec<&'static str> {
    BUILTIN.iter().filter(|c| c.critical).map(|c| c.id).collect()
}
