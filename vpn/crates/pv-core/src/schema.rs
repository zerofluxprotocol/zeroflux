//! JSON Schema export for every serialized contract.

use schemars::schema_for;

/// (file name, pretty JSON schema)
pub fn all_schemas() -> Vec<(&'static str, String)> {
    let mut v = Vec::new();
    macro_rules! s {
        ($name:literal, $t:ty) => {
            v.push(($name, serde_json::to_string_pretty(&schema_for!($t)).expect("schema")));
        };
    }
    s!("Finding.json", crate::types::Finding);
    s!("PolicyRule.json", crate::policy::PolicyRule);
    s!("PolicySet.json", crate::policy::PolicySet);
    s!("Decision.json", crate::policy::Decision);
    s!("FallbackSettings.json", crate::policy::FallbackSettings);
    s!("TransformMapEntry.json", crate::types::TransformMapEntry);
    s!("AuditEvent.json", crate::types::AuditEvent);
    s!("PendingApproval.json", crate::types::PendingApproval);
    s!("ProviderConfig.json", crate::types::ProviderConfig);
    s!("ClientIntegration.json", crate::types::ClientIntegration);
    s!("ErrorEnvelope.json", crate::errors::PvErrorEnvelope);
    v
}
