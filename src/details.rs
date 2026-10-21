//! A summary of a table's configuration: the things worth checking before touching a
//! production table, such as size, keys, indexes, deletion protection, backups and tags.

use std::collections::HashMap;

use anyhow::{Context, Result};
use aws_sdk_dynamodb::Client;
use aws_sdk_dynamodb::primitives::{DateTime, DateTimeFormat};
use aws_sdk_dynamodb::types::{KeySchemaElement, Projection, ProvisionedThroughputDescription};

use crate::ddb::sdk_err;

pub struct TableDetails {
    pub name: String,
    pub status: String,
    pub arn: String,
    /// Item count and size are estimates DynamoDB refreshes about every six hours.
    pub item_count: i64,
    pub size_bytes: i64,
    pub keys: Vec<KeyPart>,
    /// Provisioned read and write units; `None` for on-demand tables.
    pub capacity: Option<(i64, i64)>,
    /// Deletion protection, point-in-time recovery and encryption.
    pub protection: Vec<Setting>,
    /// Table class, stream, TTL and creation time.
    pub settings: Vec<Setting>,
    pub global_indexes: Vec<IndexDetails>,
    pub local_indexes: Vec<IndexDetails>,
    /// The tags, or why they could not be read.
    pub tags: Result<Vec<(String, String)>, String>,
}

pub struct KeyPart {
    pub name: String,
    pub ty: String,
    /// The sort (range) key rather than the partition (hash) key.
    pub sort: bool,
}

pub struct IndexDetails {
    pub name: String,
    pub keys: Vec<KeyPart>,
    pub projection: String,
    /// Empty for local indexes, which share the table's status.
    pub status: String,
    pub item_count: i64,
    pub size_bytes: i64,
    pub capacity: Option<(i64, i64)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    On,
    Off,
    /// A value that is neither on nor off, such as the table class.
    Plain,
    /// The lookup failed, for example for lack of permission.
    Unavailable,
}

pub struct Setting {
    pub label: &'static str,
    pub value: String,
    pub state: State,
}

impl Setting {
    fn new(label: &'static str, value: impl Into<String>, state: State) -> Self {
        Self {
            label,
            value: value.into(),
            state,
        }
    }
}

/// DescribeTable plus TTL, point-in-time recovery and tags. Only DescribeTable must succeed;
/// the other lookups report their own errors in place.
pub async fn load(client: &Client, table: &str) -> Result<TableDetails> {
    let out = client
        .describe_table()
        .table_name(table)
        .send()
        .await
        .map_err(sdk_err)?;
    let t = out.table().context("DescribeTable returned no table")?;
    let arn = t.table_arn().unwrap_or_default().to_string();
    let (ttl, pitr, tags) = tokio::join!(
        client.describe_time_to_live().table_name(table).send(),
        client
            .describe_continuous_backups()
            .table_name(table)
            .send(),
        list_tags(client, &arn),
    );

    let types: HashMap<&str, &str> = t
        .attribute_definitions()
        .iter()
        .map(|d| (d.attribute_name(), d.attribute_type().as_str()))
        .collect();
    let keys = |schema: &[KeySchemaElement]| {
        schema
            .iter()
            .map(|k| KeyPart {
                name: k.attribute_name().to_string(),
                ty: types
                    .get(k.attribute_name())
                    .copied()
                    .unwrap_or("?")
                    .to_string(),
                sort: k.key_type().as_str() == "RANGE",
            })
            .collect::<Vec<_>>()
    };

    let billing = t
        .billing_mode_summary()
        .and_then(|b| b.billing_mode())
        .map(|b| b.as_str())
        .unwrap_or("PROVISIONED");
    let table_capacity = capacity(t.provisioned_throughput()).filter(|_| billing == "PROVISIONED");

    let deletion = t.deletion_protection_enabled().unwrap_or(false);
    let pitr = match pitr {
        Ok(out) => match out
            .continuous_backups_description()
            .and_then(|d| d.point_in_time_recovery_description())
        {
            Some(p) if p.point_in_time_recovery_status().map(|s| s.as_str()) == Some("ENABLED") => {
                let value = match p.earliest_restorable_date_time() {
                    Some(d) => format!("on · restorable from {}", date(d)),
                    None => "on".to_string(),
                };
                Setting::new("Point-in-time recovery", value, State::On)
            }
            Some(_) => Setting::new("Point-in-time recovery", "off", State::Off),
            None => Setting::new("Point-in-time recovery", "?", State::Plain),
        },
        Err(e) => unavailable("Point-in-time recovery", sdk_err(e)),
    };
    let encryption = match t.sse_description() {
        None => Setting::new("Encryption", "AWS owned key (default)", State::Plain),
        Some(d) => {
            let ty = d.sse_type().map(|t| t.as_str()).unwrap_or("?");
            let status = d.status().map(|s| s.as_str()).unwrap_or("?");
            let value = match d.kms_master_key_arn() {
                Some(key) => format!("{ty} · {status} · {key}"),
                None => format!("{ty} · {status}"),
            };
            Setting::new("Encryption", value, State::On)
        }
    };
    let protection = vec![
        Setting::new(
            "Deletion protection",
            if deletion { "on" } else { "off" },
            if deletion { State::On } else { State::Off },
        ),
        pitr,
        encryption,
    ];

    let stream = match t.stream_specification() {
        Some(spec) if spec.stream_enabled() => Setting::new(
            "Stream",
            format!(
                "on · {}",
                spec.stream_view_type().map(|v| v.as_str()).unwrap_or("?")
            ),
            State::On,
        ),
        _ => Setting::new("Stream", "off", State::Off),
    };
    let ttl = match ttl {
        Ok(out) => match out.time_to_live_description() {
            Some(d) => {
                let status = d.time_to_live_status().map(|s| s.as_str()).unwrap_or("?");
                let attr = d.attribute_name().map(|a| format!(" · attribute {a}"));
                match status {
                    "ENABLED" => {
                        Setting::new("TTL", format!("on{}", attr.unwrap_or_default()), State::On)
                    }
                    "DISABLED" => Setting::new("TTL", "off", State::Off),
                    other => Setting::new(
                        "TTL",
                        format!("{}{}", other.to_lowercase(), attr.unwrap_or_default()),
                        State::Plain,
                    ),
                }
            }
            None => Setting::new("TTL", "?", State::Plain),
        },
        Err(e) => unavailable("TTL", sdk_err(e)),
    };
    let settings = vec![
        Setting::new(
            "Table class",
            t.table_class_summary()
                .and_then(|c| c.table_class())
                .map(|c| c.as_str().to_string())
                .unwrap_or_else(|| "STANDARD".into()),
            State::Plain,
        ),
        stream,
        ttl,
        Setting::new(
            "Created",
            t.creation_date_time().map(date).unwrap_or_default(),
            State::Plain,
        ),
    ];

    let global_indexes = t
        .global_secondary_indexes()
        .iter()
        .map(|g| IndexDetails {
            name: g.index_name().unwrap_or("?").to_string(),
            keys: keys(g.key_schema()),
            projection: projection(g.projection()),
            status: g
                .index_status()
                .map(|s| s.as_str().to_string())
                .unwrap_or_default(),
            item_count: g.item_count().unwrap_or(0),
            size_bytes: g.index_size_bytes().unwrap_or(0),
            capacity: capacity(g.provisioned_throughput()).filter(|_| billing == "PROVISIONED"),
        })
        .collect();
    let local_indexes = t
        .local_secondary_indexes()
        .iter()
        .map(|l| IndexDetails {
            name: l.index_name().unwrap_or("?").to_string(),
            keys: keys(l.key_schema()),
            projection: projection(l.projection()),
            status: String::new(),
            item_count: l.item_count().unwrap_or(0),
            size_bytes: l.index_size_bytes().unwrap_or(0),
            capacity: None,
        })
        .collect();

    Ok(TableDetails {
        name: table.to_string(),
        status: t
            .table_status()
            .map(|s| s.as_str().to_string())
            .unwrap_or_else(|| "?".into()),
        arn,
        item_count: t.item_count().unwrap_or(0),
        size_bytes: t.table_size_bytes().unwrap_or(0),
        keys: keys(t.key_schema()),
        capacity: table_capacity,
        protection,
        settings,
        global_indexes,
        local_indexes,
        tags: tags.map_err(|e| format!("{e:#}")),
    })
}

fn unavailable(label: &'static str, e: anyhow::Error) -> Setting {
    Setting::new(label, format!("unavailable: {e:#}"), State::Unavailable)
}

async fn list_tags(client: &Client, arn: &str) -> Result<Vec<(String, String)>> {
    let mut tags = Vec::new();
    let mut token = None;
    loop {
        let out = client
            .list_tags_of_resource()
            .resource_arn(arn)
            .set_next_token(token)
            .send()
            .await
            .map_err(sdk_err)?;
        tags.extend(
            out.tags()
                .iter()
                .map(|t| (t.key().to_string(), t.value().to_string())),
        );
        token = out.next_token().map(str::to_string);
        if token.is_none() {
            tags.sort();
            return Ok(tags);
        }
    }
}

/// Provisioned read/write units; on-demand tables report zeros, which mean none.
fn capacity(p: Option<&ProvisionedThroughputDescription>) -> Option<(i64, i64)> {
    let p = p?;
    let (r, w) = (
        p.read_capacity_units().unwrap_or(0),
        p.write_capacity_units().unwrap_or(0),
    );
    (r > 0 || w > 0).then_some((r, w))
}

fn projection(p: Option<&Projection>) -> String {
    let Some(p) = p else { return "?".into() };
    let ty = p.projection_type().map(|t| t.as_str()).unwrap_or("?");
    if p.non_key_attributes().is_empty() {
        ty.to_string()
    } else {
        format!("{ty} ({})", p.non_key_attributes().join(", "))
    }
}

/// `2026-10-21 05:36 UTC`.
fn date(d: &DateTime) -> String {
    let full = d.fmt(DateTimeFormat::DateTime).unwrap_or_default();
    match full.get(..16) {
        Some(minutes) => format!("{} UTC", minutes.replace('T', " ")),
        None => full,
    }
}

pub fn bytes(n: i64) -> String {
    let n = n as f64;
    match n {
        n if n >= 1e9 => format!("{:.1} GB", n / 1e9),
        n if n >= 1e6 => format!("{:.1} MB", n / 1e6),
        n if n >= 1e3 => format!("{:.1} KB", n / 1e3),
        n => format!("{n} B"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_readable() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(12_300), "12.3 KB");
        assert_eq!(bytes(4_500_000_000), "4.5 GB");
    }

    #[test]
    fn projections_list_included_attributes() {
        use aws_sdk_dynamodb::types::ProjectionType;
        let p = Projection::builder()
            .projection_type(ProjectionType::Include)
            .non_key_attributes("a")
            .non_key_attributes("b")
            .build();
        assert_eq!(projection(Some(&p)), "INCLUDE (a, b)");
        assert_eq!(projection(None), "?");
    }

    #[test]
    fn dates_read_to_the_minute() {
        let d = DateTime::from_secs(1_792_560_998);
        assert_eq!(date(&d), "2026-10-21 05:36 UTC");
    }
}
