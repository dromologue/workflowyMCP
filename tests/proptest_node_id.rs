//! Property-based tests for `node_id` shapes.
//!
//! Brief 1.1 hypothesised that intermittent `null` deserialisation
//! errors might come from the server side. The Pass-1 triage showed
//! they are client-side: the server's `Parameters<T>` deserialiser
//! correctly rejects nulls for required string fields. These tests
//! exercise the validator and short-hash boundary so we can prove
//! the intended behaviour holds under random input.
//!
//! Tests run as part of `cargo test` — no live API needed.

use proptest::prelude::*;
use workflowy_mcp_server::types::NodeId;
use workflowy_mcp_server::validation::validate_node_id;

/// 32-char lowercase-hex strings — the unhyphenated UUID form
/// Workflowy accepts.
fn arb_unhyphenated_uuid() -> impl Strategy<Value = String> {
    proptest::collection::vec(prop::sample::select(b"0123456789abcdef".to_vec()), 32)
        .prop_map(|chars| chars.into_iter().map(|b| b as char).collect())
}

/// 8-4-4-4-12 hyphenated UUIDs.
fn arb_hyphenated_uuid() -> impl Strategy<Value = String> {
    arb_unhyphenated_uuid().prop_map(|s| {
        format!(
            "{}-{}-{}-{}-{}",
            &s[..8],
            &s[8..12],
            &s[12..16],
            &s[16..20],
            &s[20..]
        )
    })
}

/// Random short strings that should NOT validate as full UUIDs.
fn arb_garbage() -> impl Strategy<Value = String> {
    "[a-z0-9]{0,16}"
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, .. ProptestConfig::default() })]

    #[test]
    fn unhyphenated_uuid_always_validates(s in arb_unhyphenated_uuid()) {
        prop_assert!(validate_node_id(&s).is_ok(), "{s} should validate");
    }

    #[test]
    fn hyphenated_uuid_always_validates(s in arb_hyphenated_uuid()) {
        prop_assert!(validate_node_id(&s).is_ok(), "{s} should validate");
    }

    #[test]
    fn garbage_strings_never_panic(s in arb_garbage()) {
        // The validator must never panic on arbitrary input — only return
        // Ok(()) or Err(InvalidInput). Confirms the deserialiser cannot
        // be DoS'd into a panic by malformed strings.
        let _ = validate_node_id(&s);
    }

    #[test]
    fn node_id_roundtrip_via_serde_transparent(s in arb_unhyphenated_uuid()) {
        // NodeId is `#[serde(transparent)]` over String — JSON encoded
        // form is just the bare string. Confirm the round-trip is
        // lossless across random valid UUIDs.
        let id = NodeId::from(s.clone());
        let json = serde_json::to_string(&id).unwrap();
        prop_assert_eq!(json, format!("\"{}\"", s));
        let back: NodeId = serde_json::from_str(&format!("\"{}\"", s)).unwrap();
        prop_assert_eq!(back.as_str(), &s);
    }

    #[test]
    fn null_is_always_rejected_at_serde_layer(_dummy in 0u8..1) {
        // The original brief asked for a server-side fix to `null` ids.
        // The triage showed serde already does the right thing — confirm
        // it here so a future refactor can't silently regress.
        let result: Result<NodeId, _> = serde_json::from_str("null");
        prop_assert!(result.is_err());
    }
}
