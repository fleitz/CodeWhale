//! Behavior tests for the route foundation (#2608 / #3084 / #3384).

use super::RequestProtocol;
use super::descriptor::ProviderDescriptor;
use super::ids::{LogicalModelRef, ModelId, NamespaceHint, ProviderId, WireModelId};
use crate::ProviderKind;

#[test]
fn provider_id_from_kind_uses_canonical_id() {
    assert_eq!(
        ProviderId::from_kind(ProviderKind::Deepseek).as_str(),
        "deepseek"
    );
    assert_eq!(
        ProviderId::from_kind(ProviderKind::Openrouter).as_str(),
        "openrouter"
    );
}

#[test]
fn model_id_and_wire_model_id_are_distinct_types() {
    // This test asserts the values; the *type* distinction is enforced by the
    // compiler: a function taking `WireModelId` rejects a `ModelId` argument.
    let canonical = ModelId::from("deepseek-v4-pro");
    let wire = WireModelId::from("deepseek-ai/DeepSeek-V4-Pro");
    assert_eq!(canonical.as_str(), "deepseek-v4-pro");
    assert_eq!(wire.as_str(), "deepseek-ai/DeepSeek-V4-Pro");
}

#[test]
fn logical_model_ref_auto_is_sentinel() {
    assert!(LogicalModelRef::from("auto").is_auto());
    assert!(!LogicalModelRef::from("deepseek-v4-pro").is_auto());
}

#[test]
fn logical_model_ref_namespace_hint_parses_curated_prefixes() {
    let cases = [
        ("deepseek-ai/DeepSeek-V4-Pro", NamespaceHint::DeepseekAi),
        ("deepseek/deepseek-v4-pro", NamespaceHint::Deepseek),
        ("anthropic/claude-foo", NamespaceHint::Anthropic),
        ("openai/gpt-foo", NamespaceHint::Openai),
        ("qwen/qwen-foo", NamespaceHint::Qwen),
    ];
    for (raw, expected) in cases {
        assert_eq!(
            LogicalModelRef::from(raw).namespace_hint(),
            Some(expected),
            "{raw} should parse to {expected:?}"
        );
    }
    assert_eq!(LogicalModelRef::from("plain-model").namespace_hint(), None);
    assert_eq!(LogicalModelRef::from("auto").namespace_hint(), None);
}

/// By construction there is NO path from a namespace prefix to a provider.
///
/// This is enforced by the *absence* of any `From<NamespaceHint>` /
/// `From<LogicalModelRef>` for `ProviderId`. The following lines are the
/// canonical way to mint a `ProviderId` and demonstrate the only supported
/// source is an explicit `ProviderKind`, never a parsed prefix.
#[test]
fn no_namespace_hint_or_logical_ref_to_provider_id_conversion() {
    let hint = LogicalModelRef::from("deepseek-ai/DeepSeek-V4-Pro").namespace_hint();
    assert_eq!(hint, Some(NamespaceHint::DeepseekAi));
    // A ProviderId may ONLY be built from an explicit ProviderKind or string,
    // never derived from the hint above. (If a `From<NamespaceHint>` for
    // `ProviderId` were ever added, this seam would silently break #2608.)
    let provider = ProviderId::from_kind(ProviderKind::Together);
    assert_eq!(provider.as_str(), "together");
}

#[test]
fn newtypes_serialize_transparently() {
    let id = ProviderId::from("deepseek");
    assert_eq!(serde_json::to_string(&id).unwrap(), "\"deepseek\"");
    let wire = WireModelId::from("deepseek-ai/DeepSeek-V4-Pro");
    assert_eq!(
        serde_json::to_string(&wire).unwrap(),
        "\"deepseek-ai/DeepSeek-V4-Pro\""
    );
}

#[test]
fn descriptor_for_every_kind_has_nonempty_transport_facts() {
    for kind in ProviderKind::ALL {
        let d = ProviderDescriptor::for_kind(kind);
        assert!(!d.id().as_str().is_empty(), "{kind:?} id empty");
        assert!(
            !d.default_base_url().is_empty(),
            "{kind:?} default_base_url empty"
        );
        assert!(
            !d.default_wire_model().as_str().is_empty(),
            "{kind:?} default_wire_model empty"
        );
        // protocol() always yields a RequestProtocol; calling it must not panic.
        let _: RequestProtocol = d.protocol();
    }
}

#[test]
fn descriptor_protocol_matches_provider_wire() {
    for kind in ProviderKind::ALL {
        let d = ProviderDescriptor::for_kind(kind);
        assert_eq!(
            d.protocol(),
            kind.provider().wire(),
            "{kind:?} protocol must equal provider().wire()"
        );
        let expected = match kind {
            ProviderKind::OpenaiCodex => RequestProtocol::Responses,
            ProviderKind::Anthropic => RequestProtocol::AnthropicMessages,
            _ => RequestProtocol::ChatCompletions,
        };
        assert_eq!(d.protocol(), expected, "{kind:?} protocol mismatch");
    }
}
