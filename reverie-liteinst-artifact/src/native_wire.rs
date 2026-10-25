// Copyright (c) Meta Platforms, Inc. and affiliates.
// SPDX-License-Identifier: BSD-2-Clause

//! Bounded JSON decoding for untrusted native result records. Duplicate keys
//! are rejected even inside payloads that a particular reader leaves opaque.

use std::fmt;

use serde::Deserialize;
use serde::Deserializer;
use serde::de::Error;
use serde::de::MapAccess;
use serde::de::SeqAccess;
use serde::de::Visitor;
use serde_json::Map;
use serde_json::Number;
use serde_json::Value;

struct UniqueValue(Value);

impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Unique;
        impl<'de> Visitor<'de> for Unique {
            type Value = UniqueValue;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("JSON with unique object keys")
            }

            fn visit_bool<E: Error>(self, value: bool) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::Bool(value)))
            }

            fn visit_i64<E: Error>(self, value: i64) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::Number(value.into())))
            }

            fn visit_u64<E: Error>(self, value: u64) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::Number(value.into())))
            }

            fn visit_f64<E: Error>(self, value: f64) -> Result<Self::Value, E> {
                Number::from_f64(value)
                    .map(|number| UniqueValue(Value::Number(number)))
                    .ok_or_else(|| E::custom("non-finite JSON number"))
            }

            fn visit_str<E: Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::String(value.to_owned())))
            }

            fn visit_string<E: Error>(self, value: String) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::String(value)))
            }

            fn visit_unit<E: Error>(self) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::Null))
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut values: A) -> Result<Self::Value, A::Error> {
                let mut result = Vec::new();
                while let Some(UniqueValue(value)) = values.next_element()? {
                    result.push(value);
                }
                Ok(UniqueValue(Value::Array(result)))
            }

            fn visit_map<A: MapAccess<'de>>(self, mut values: A) -> Result<Self::Value, A::Error> {
                let mut result = Map::new();
                while let Some((key, UniqueValue(value))) =
                    values.next_entry::<String, UniqueValue>()?
                {
                    if result.insert(key.clone(), value).is_some() {
                        return Err(A::Error::custom(format!("duplicate JSON key: {key}")));
                    }
                }
                Ok(UniqueValue(Value::Object(result)))
            }
        }
        deserializer.deserialize_any(Unique)
    }
}

// serde_json's ordinary visitor can convert an out-of-range integer token to
// f64 before visit_f64 sees it. Refuse that lossy conversion while the original
// lexeme is still available. Strings are skipped, and decimal/exponent forms
// retain the existing floating-point decoding. The normal decoder remains the
// grammar authority, including for malformed tokens and unterminated strings.
fn validate_integer_lexemes(bytes: &[u8]) -> super::Result<()> {
    let mut offset = 0;
    while let Some(&byte) = bytes.get(offset) {
        match byte {
            b'"' => {
                offset += 1;
                while let Some(&byte) = bytes.get(offset) {
                    offset += 1;
                    match byte {
                        b'\\' => offset += 1,
                        b'"' => break,
                        _ => {}
                    }
                }
            }
            b'-' | b'0'..=b'9' => {
                let start = offset;
                offset += 1;
                while bytes.get(offset).is_some_and(|byte| {
                    matches!(byte, b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')
                }) {
                    offset += 1;
                }
                let token = &bytes[start..offset];
                if !token.iter().any(|byte| matches!(byte, b'.' | b'e' | b'E')) {
                    // The scanner admits only ASCII number characters here.
                    let integer = std::str::from_utf8(token)
                        .map_err(|e| format!("native result integer lexeme: {e}"))?;
                    let supported = if integer.starts_with('-') {
                        integer.parse::<i64>().is_ok()
                    } else {
                        integer.parse::<u64>().is_ok()
                    };
                    super::require(
                        supported,
                        "native result integer lexeme exceeds supported i64/u64 range",
                    )?;
                }
            }
            _ => offset += 1,
        }
    }
    Ok(())
}

pub(super) fn parse(bytes: &[u8], max_bytes: usize) -> super::Result<Value> {
    super::require(
        bytes.len() <= max_bytes,
        "native result record exceeds byte bound",
    )?;
    validate_integer_lexemes(bytes)?;
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let UniqueValue(value) =
        UniqueValue::deserialize(&mut decoder).map_err(|e| format!("native result JSON: {e}"))?;
    decoder
        .end()
        .map_err(|e| format!("native result JSON: {e}"))?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn record_bound_and_nested_duplicate_keys_refuse_without_truncation() {
        let bytes = br#"{"values":[null,true,-4,5,1.5,"text",{"key":1}]}"#;
        assert!(parse(bytes, bytes.len()).is_ok());
        assert!(parse(bytes, bytes.len() - 1).is_err());
        assert!(parse(br#"{"opaque":[{"key":null,"key":false}]}"#, 1024).is_err());
        assert!(parse(b"{} {}", 1024).is_err());
    }

    #[test]
    fn review_f6_integer_lexemes_are_exact_or_refused_probe() {
        let mut lossy = Vec::new();
        for (literal, must_accept) in [
            ("18446744073709551615", true),
            ("-9223372036854775808", true),
            ("18446744073709551617", false),
            ("-9223372036854775809", false),
        ] {
            match parse(literal.as_bytes(), 1024) {
                Ok(value) => {
                    let serialized = serde_json::to_string(&value).unwrap();
                    eprintln!(
                        "F6_INTEGER_PROBE input={literal} accepted=true serialized={serialized}"
                    );
                    if serialized != literal {
                        lossy.push(literal);
                    }
                }
                Err(error) => {
                    eprintln!("F6_INTEGER_PROBE input={literal} accepted=false error={error}");
                    assert!(
                        !must_accept,
                        "supported integer refused: {literal}: {error}"
                    );
                }
            }
        }
        assert!(
            lossy.is_empty(),
            "integer lexemes silently changed: {lossy:?}"
        );
    }

    #[test]
    fn integer_token_walk_preserves_strings_escapes_and_existing_float_forms() {
        for bytes in [
            br#"{"text":"18446744073709551617","negative":"-9223372036854775809"}"#.as_slice(),
            br#"{"text":"escaped quote: \" 18446744073709551617","n":7}"#.as_slice(),
            br#"["\\","\u003118446744073709551617",1.5,-2.25,1e20,2E-3]"#.as_slice(),
            br#"{"fraction":18446744073709551617.5,"exponent":18446744073709551617e0}"#.as_slice(),
            br#"[-9223372036854775808,18446744073709551615,0,-0]"#.as_slice(),
        ] {
            assert_eq!(
                parse(bytes, 4096).unwrap(),
                serde_json::from_slice::<serde_json::Value>(bytes).unwrap()
            );
        }
    }

    #[test]
    fn integer_token_walk_rejects_nested_overflow_and_never_repairs_bad_json() {
        for bytes in [
            br#"{"opaque":[18446744073709551617]}"#.as_slice(),
            br#"{"text":"escaped quote: \"","opaque":-9223372036854775809}"#.as_slice(),
            br#"["\\",18446744073709551617]"#.as_slice(),
        ] {
            assert!(
                parse(bytes, 4096)
                    .unwrap_err()
                    .contains("integer lexeme exceeds supported i64/u64 range")
            );
        }
        for bytes in [
            b"01".as_slice(),
            b"1e".as_slice(),
            b"1.2.3".as_slice(),
            b"--1".as_slice(),
            b"1 2".as_slice(),
            br#""unterminated \""#.as_slice(),
        ] {
            assert!(parse(bytes, 4096).is_err(), "{bytes:?}");
        }
    }
}
