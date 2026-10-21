//! Conversions between DynamoDB attribute values and JSON.

use std::collections::HashMap;

use anyhow::{Result, anyhow, bail};
use aws_sdk_dynamodb::primitives::Blob;
use aws_sdk_dynamodb::types::AttributeValue;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde_json::{Value, json};

pub type Item = HashMap<String, AttributeValue>;

/// DynamoDB JSON (`{"S": "x"}`). Lossless, so it is the format for viewing and editing.
pub fn to_json(v: &AttributeValue) -> Value {
    match v {
        AttributeValue::S(s) => json!({ "S": s }),
        AttributeValue::N(n) => json!({ "N": n }),
        AttributeValue::B(b) => json!({ "B": B64.encode(b.as_ref()) }),
        AttributeValue::Bool(b) => json!({ "BOOL": b }),
        AttributeValue::Null(b) => json!({ "NULL": b }),
        AttributeValue::Ss(v) => json!({ "SS": v }),
        AttributeValue::Ns(v) => json!({ "NS": v }),
        AttributeValue::Bs(v) => {
            json!({ "BS": v.iter().map(|b| B64.encode(b.as_ref())).collect::<Vec<_>>() })
        }
        AttributeValue::L(v) => json!({ "L": v.iter().map(to_json).collect::<Vec<_>>() }),
        AttributeValue::M(m) => json!({ "M": item_to_json(m) }),
        _ => json!({ "UNKNOWN": null }),
    }
}

pub fn item_to_json(item: &Item) -> Value {
    Value::Object(item.iter().map(|(k, v)| (k.clone(), to_json(v))).collect())
}

pub fn from_json(v: &Value) -> Result<AttributeValue> {
    let obj = v
        .as_object()
        .filter(|o| o.len() == 1)
        .ok_or_else(|| anyhow!("expected a type object such as {{\"S\": \"...\"}}, got {v}"))?;
    let (ty, val) = obj.iter().next().expect("checked length");
    Ok(match (ty.as_str(), val) {
        ("S", Value::String(s)) => AttributeValue::S(s.clone()),
        ("N", Value::String(n)) => AttributeValue::N(n.clone()),
        ("B", Value::String(s)) => AttributeValue::B(Blob::new(B64.decode(s)?)),
        ("BOOL", Value::Bool(b)) => AttributeValue::Bool(*b),
        ("NULL", Value::Bool(b)) => AttributeValue::Null(*b),
        ("SS", Value::Array(a)) => AttributeValue::Ss(strings(a, ty)?),
        ("NS", Value::Array(a)) => AttributeValue::Ns(strings(a, ty)?),
        ("BS", Value::Array(a)) => AttributeValue::Bs(
            strings(a, ty)?
                .iter()
                .map(|s| Ok(Blob::new(B64.decode(s)?)))
                .collect::<Result<_>>()?,
        ),
        ("L", Value::Array(a)) => {
            AttributeValue::L(a.iter().map(from_json).collect::<Result<_>>()?)
        }
        ("M", Value::Object(_)) => AttributeValue::M(item_from_json(val)?),
        _ => bail!("invalid {ty} value: {val} (N, NS and B values are strings)"),
    })
}

pub fn item_from_json(v: &Value) -> Result<Item> {
    let obj = v
        .as_object()
        .ok_or_else(|| anyhow!("an item must be a JSON object"))?;
    obj.iter()
        .map(|(k, v)| Ok((k.clone(), from_json(v).map_err(|e| anyhow!("{k}: {e}"))?)))
        .collect()
}

fn strings(a: &[Value], ty: &str) -> Result<Vec<String>> {
    a.iter()
        .map(|x| {
            x.as_str()
                .map(str::to_string)
                .ok_or_else(|| anyhow!("{ty} elements must be strings"))
        })
        .collect()
}

/// Plain JSON for compact display; sets become arrays and binary becomes base64.
fn plain(v: &AttributeValue) -> Value {
    match v {
        AttributeValue::S(s) => Value::String(s.clone()),
        AttributeValue::N(n) => n
            .parse()
            .map(Value::Number)
            .unwrap_or_else(|_| Value::String(n.clone())),
        AttributeValue::B(b) => Value::String(B64.encode(b.as_ref())),
        AttributeValue::Bool(b) => Value::Bool(*b),
        AttributeValue::Null(_) => Value::Null,
        AttributeValue::Ss(v) => json!(v),
        AttributeValue::Ns(v) => Value::Array(
            v.iter()
                .map(|n| plain(&AttributeValue::N(n.clone())))
                .collect(),
        ),
        AttributeValue::Bs(v) => {
            json!(v.iter().map(|b| B64.encode(b.as_ref())).collect::<Vec<_>>())
        }
        AttributeValue::L(v) => Value::Array(v.iter().map(plain).collect()),
        AttributeValue::M(m) => {
            Value::Object(m.iter().map(|(k, v)| (k.clone(), plain(v))).collect())
        }
        _ => Value::Null,
    }
}

/// Plain JSON (`{"name": "x", "n": 1}`) for copying into code or documents.
pub fn item_to_plain_json(item: &Item) -> Value {
    Value::Object(item.iter().map(|(k, v)| (k.clone(), plain(v))).collect())
}

/// Strings and numbers as-is, everything else as compact plain JSON.
pub fn text(v: &AttributeValue) -> String {
    match v {
        AttributeValue::S(s) => s.clone(),
        AttributeValue::N(n) => n.clone(),
        other => plain(other).to_string(),
    }
}

/// One-line text for a table cell.
pub fn cell(v: &AttributeValue) -> String {
    text(v).replace(['\n', '\r', '\t'], " ")
}

/// Key value typed into the query form, converted by the key's attribute type.
pub fn scalar(ty: &str, text: &str) -> Result<AttributeValue> {
    match ty {
        "S" => Ok(AttributeValue::S(text.to_string())),
        "N" => {
            let n = text.trim();
            n.parse::<f64>()
                .map_err(|_| anyhow!("'{text}' is not a number"))?;
            Ok(AttributeValue::N(n.to_string()))
        }
        "B" => Ok(AttributeValue::B(Blob::new(B64.decode(text.trim())?))),
        _ => bail!("unsupported key type {ty}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Item {
        let nested = Item::from([("inner".to_string(), AttributeValue::N("1.50".into()))]);
        Item::from([
            ("s".to_string(), AttributeValue::S("line1\nline2".into())),
            (
                "n".to_string(),
                AttributeValue::N("12345678901234567890".into()),
            ),
            (
                "b".to_string(),
                AttributeValue::B(Blob::new(vec![0, 1, 255])),
            ),
            ("t".to_string(), AttributeValue::Bool(true)),
            ("z".to_string(), AttributeValue::Null(true)),
            (
                "ss".to_string(),
                AttributeValue::Ss(vec!["a".into(), "b".into()]),
            ),
            (
                "ns".to_string(),
                AttributeValue::Ns(vec!["1".into(), "2.5".into()]),
            ),
            (
                "bs".to_string(),
                AttributeValue::Bs(vec![Blob::new(vec![7])]),
            ),
            (
                "l".to_string(),
                AttributeValue::L(vec![
                    AttributeValue::S("x".into()),
                    AttributeValue::M(nested.clone()),
                ]),
            ),
            ("m".to_string(), AttributeValue::M(nested)),
        ])
    }

    #[test]
    fn dynamodb_json_round_trips_every_type() {
        let item = sample();
        assert_eq!(item_from_json(&item_to_json(&item)).unwrap(), item);
    }

    #[test]
    fn large_numbers_keep_their_digits() {
        let json = item_to_json(&sample());
        assert_eq!(json["n"], json!({ "N": "12345678901234567890" }));
    }

    #[test]
    fn rejects_malformed_values_with_the_attribute_path() {
        let err = item_from_json(&json!({ "m": { "M": { "x": { "N": 5 } } } })).unwrap_err();
        assert!(
            err.to_string().starts_with("m: x: invalid N value"),
            "{err}"
        );
        assert!(item_from_json(&json!({ "a": "plain" })).is_err());
        assert!(item_from_json(&json!({ "a": { "S": "x", "N": "1" } })).is_err());
    }

    #[test]
    fn cells_are_single_line() {
        let item = sample();
        assert_eq!(cell(&item["s"]), "line1 line2");
        assert_eq!(cell(&item["m"]), r#"{"inner":1.50}"#);
        assert_eq!(cell(&item["ss"]), r#"["a","b"]"#);
    }

    #[test]
    fn scalar_validates_numbers() {
        assert_eq!(scalar("N", " 42 ").unwrap(), AttributeValue::N("42".into()));
        assert!(scalar("N", "abc").is_err());
        assert_eq!(scalar("S", "a b").unwrap(), AttributeValue::S("a b".into()));
    }
}
