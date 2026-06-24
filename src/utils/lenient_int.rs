//! Lenient integer deserialisation for MCP parameter structs.
//!
//! Some MCP hosts serialise integer-typed tool parameters as JSON *strings*
//! (`"3100"` instead of `3100`). The serde-derived schema for an `i32` /
//! `usize` / `i64` / `u64` field then rejects the payload at the wire with
//! MCP error -32602 (`invalid type: string "3100", expected i32`), and the
//! caller's only workaround is to omit the parameter entirely — silently
//! capping what a write can express (the 2026-06-24 Cowork-host incident:
//! `create_node.priority` and `get_subtree.max_depth` both rejected).
//!
//! These helpers accept EITHER a JSON number OR a string holding a number and
//! coerce to the target integer type. For optional fields, `null` and the
//! empty string map to `None`. Applied via
//! `#[serde(deserialize_with = "…")]` on every integer MCP parameter so the
//! host quirk is param-agnostic — see `server::params`.
//!
//! The published JSON Schema is unchanged (still `integer`): schemars reads
//! the field's Rust type, not the serde `deserialize_with`. The leniency is a
//! runtime tolerance for a host that violates the schema, not a schema change.

use std::fmt;
use std::marker::PhantomData;
use std::str::FromStr;

use serde::de::{self, Deserializer, Visitor};

/// Visitor accepting a JSON integer or a string containing an integer, for a
/// required field. `T` is any integer type that is `TryFrom<i64>` +
/// `TryFrom<u64>` + `FromStr` — every fixed-width integer in std qualifies.
struct IntOrStr<T>(PhantomData<T>);

impl<'de, T> Visitor<'de> for IntOrStr<T>
where
    T: TryFrom<i64> + TryFrom<u64> + FromStr,
    <T as FromStr>::Err: fmt::Display,
{
    type Value = T;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("an integer or a string containing an integer")
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<T, E> {
        T::try_from(v).map_err(|_| E::custom(format!("integer {v} out of range for target type")))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<T, E> {
        T::try_from(v).map_err(|_| E::custom(format!("integer {v} out of range for target type")))
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<T, E> {
        v.trim()
            .parse::<T>()
            .map_err(|e| E::custom(format!("invalid integer string {v:?}: {e}")))
    }
}

/// Deserialize a **required** integer that may arrive as a JSON number or a
/// stringified number.
pub fn de_string_or_int<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: TryFrom<i64> + TryFrom<u64> + FromStr,
    <T as FromStr>::Err: fmt::Display,
{
    deserializer.deserialize_any(IntOrStr(PhantomData))
}

/// Visitor for an **optional** integer. `null` and empty/whitespace-only
/// strings map to `None`; a number or non-empty numeric string maps to
/// `Some`.
struct OptIntOrStr<T>(PhantomData<T>);

impl<'de, T> Visitor<'de> for OptIntOrStr<T>
where
    T: TryFrom<i64> + TryFrom<u64> + FromStr,
    <T as FromStr>::Err: fmt::Display,
{
    type Value = Option<T>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("an integer, a string containing an integer, null, or an empty string")
    }

    fn visit_unit<E: de::Error>(self) -> Result<Option<T>, E> {
        Ok(None)
    }

    fn visit_none<E: de::Error>(self) -> Result<Option<T>, E> {
        Ok(None)
    }

    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Option<T>, D::Error> {
        d.deserialize_any(self)
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Option<T>, E> {
        IntOrStr(PhantomData).visit_i64(v).map(Some)
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Option<T>, E> {
        IntOrStr(PhantomData).visit_u64(v).map(Some)
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Option<T>, E> {
        if v.trim().is_empty() {
            Ok(None)
        } else {
            IntOrStr(PhantomData).visit_str(v).map(Some)
        }
    }
}

/// Deserialize an **optional** integer that may arrive as a JSON number, a
/// stringified number, `null`, or an empty string. Pair with
/// `#[serde(default, …)]` so an omitted field also resolves to `None`.
pub fn de_opt_string_or_int<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: TryFrom<i64> + TryFrom<u64> + FromStr,
    <T as FromStr>::Err: fmt::Display,
{
    deserializer.deserialize_any(OptIntOrStr(PhantomData))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq)]
    struct Req {
        #[serde(deserialize_with = "de_string_or_int")]
        v: i64,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    struct Opt {
        #[serde(default, deserialize_with = "de_opt_string_or_int")]
        v: Option<usize>,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    struct OptPriority {
        #[serde(default, deserialize_with = "de_opt_string_or_int")]
        v: Option<i32>,
    }

    #[test]
    fn string_and_int_deserialise_to_the_same_required_value() {
        let from_str: Req = serde_json::from_str(r#"{"v":"3100"}"#).unwrap();
        let from_int: Req = serde_json::from_str(r#"{"v":3100}"#).unwrap();
        assert_eq!(from_str, from_int);
        assert_eq!(from_str.v, 3100);
    }

    #[test]
    fn string_and_int_deserialise_to_the_same_optional_value() {
        // The exact 2026-06-24 Cowork-host shapes: get_subtree max_depth="2".
        let from_str: Opt = serde_json::from_str(r#"{"v":"2"}"#).unwrap();
        let from_int: Opt = serde_json::from_str(r#"{"v":2}"#).unwrap();
        assert_eq!(from_str, from_int);
        assert_eq!(from_str.v, Some(2));
    }

    #[test]
    fn empty_string_and_null_and_omitted_map_to_none() {
        let empty: Opt = serde_json::from_str(r#"{"v":""}"#).unwrap();
        let whitespace: Opt = serde_json::from_str(r#"{"v":"  "}"#).unwrap();
        let null: Opt = serde_json::from_str(r#"{"v":null}"#).unwrap();
        let omitted: Opt = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(empty.v, None);
        assert_eq!(whitespace.v, None);
        assert_eq!(null.v, None);
        assert_eq!(omitted.v, None);
    }

    #[test]
    fn negative_priority_string_round_trips() {
        // create_node priority is signed; a stringified negative must survive.
        let from_str: OptPriority = serde_json::from_str(r#"{"v":"-5"}"#).unwrap();
        let from_int: OptPriority = serde_json::from_str(r#"{"v":-5}"#).unwrap();
        assert_eq!(from_str, from_int);
        assert_eq!(from_str.v, Some(-5));
    }

    #[test]
    fn whitespace_padded_numeric_string_parses() {
        let padded: Opt = serde_json::from_str(r#"{"v":" 7 "}"#).unwrap();
        assert_eq!(padded.v, Some(7));
    }

    #[test]
    fn non_numeric_string_is_rejected() {
        let err = serde_json::from_str::<Opt>(r#"{"v":"abc"}"#);
        assert!(err.is_err());
    }

    #[test]
    fn out_of_range_for_target_type_is_rejected() {
        // usize cannot hold a negative; the string path must fail, not wrap.
        let err = serde_json::from_str::<Opt>(r#"{"v":"-1"}"#);
        assert!(err.is_err());
    }
}
