//! A readable summary of a table's configuration: the things worth checking before touching
//! a production table, such as deletion protection, encryption, TTL, backups and tags.

use std::collections::HashMap;
use std::fmt::Write as _;

use anyhow::{Context, Result};
use aws_sdk_dynamodb::Client;
use aws_sdk_dynamodb::primitives::{DateTime, DateTimeFormat};
use aws_sdk_dynamodb::types::{KeySchemaElement, Projection, ProvisionedThroughputDescription};

use crate::ddb::sdk_err;

/// DescribeTable plus TTL, point-in-time recovery and tags. Only DescribeTable must succeed;
/// the other lookups report their own errors inline (for example a missing permission).
pub async fn report(client: &Client, table: &str) -> Result<String> {
    let out = client
        .describe_table()
        .table_name(table)
        .send()
        .await
        .map_err(sdk_err)?;
    let t = out.table().context("DescribeTable returned no table")?;
    let arn = t.table_arn().unwrap_or_default();
    let (ttl, pitr, tags) = tokio::join!(
        client.describe_time_to_live().table_name(table).send(),
        client
            .describe_continuous_backups()
            .table_name(table)
            .send(),
        list_tags(client, arn),
    );

    let types: HashMap<&str, &str> = t
        .attribute_definitions()
        .iter()
        .map(|d| (d.attribute_name(), d.attribute_type().as_str()))
        .collect();
    let keys = |schema: &[KeySchemaElement]| {
        schema
            .iter()
            .map(|k| {
                let ty = types.get(k.attribute_name()).copied().unwrap_or("?");
                format!("{} ({ty}, {})", k.attribute_name(), k.key_type().as_str())
            })
            .collect::<Vec<_>>()
            .join(" · ")
    };

    let mut s = String::new();
    let mut row = |label: &str, value: String| {
        let _ = writeln!(s, " {label:<24}{value}");
    };
    let status = t.table_status().map(|s| s.as_str()).unwrap_or("?");
    row("Table", format!("{table}  ({status})"));
    row("ARN", arn.to_string());
    row(
        "Items · size",
        format!(
            "~{} items · {}  (DynamoDB refreshes these about every 6 hours)",
            t.item_count().unwrap_or(0),
            bytes(t.table_size_bytes().unwrap_or(0))
        ),
    );
    row("Keys", keys(t.key_schema()));
    let billing = t
        .billing_mode_summary()
        .and_then(|b| b.billing_mode())
        .map(|b| b.as_str())
        .unwrap_or("PROVISIONED");
    let billing = match capacity(t.provisioned_throughput()) {
        Some(c) if billing == "PROVISIONED" => format!("{billing} · {c}"),
        _ => billing.to_string(),
    };
    row("Billing", billing);
    row(
        "Table class",
        t.table_class_summary()
            .and_then(|c| c.table_class())
            .map(|c| c.as_str().to_string())
            .unwrap_or_else(|| "STANDARD".into()),
    );
    row(
        "Deletion protection",
        on_off(t.deletion_protection_enabled().unwrap_or(false)),
    );
    let encryption = match t.sse_description() {
        None => "AWS owned key (default)".to_string(),
        Some(d) => {
            let ty = d.sse_type().map(|t| t.as_str()).unwrap_or("?");
            let status = d.status().map(|s| s.as_str()).unwrap_or("?");
            match d.kms_master_key_arn() {
                Some(key) => format!("{ty} · {status} · {key}"),
                None => format!("{ty} · {status}"),
            }
        }
    };
    row("Encryption", encryption);
    let stream = match t.stream_specification() {
        Some(spec) if spec.stream_enabled() => format!(
            "on · {}",
            spec.stream_view_type().map(|v| v.as_str()).unwrap_or("?")
        ),
        _ => "off".to_string(),
    };
    row("Stream", stream);
    let ttl = match ttl {
        Ok(out) => match out.time_to_live_description() {
            Some(d) => {
                let status = d.time_to_live_status().map(|s| s.as_str()).unwrap_or("?");
                match d.attribute_name() {
                    Some(attr) => format!("{status} · attribute {attr}"),
                    None => status.to_string(),
                }
            }
            None => "?".to_string(),
        },
        Err(e) => format!("unavailable: {:#}", sdk_err(e)),
    };
    row("TTL", ttl);
    let pitr = match pitr {
        Ok(out) => match out
            .continuous_backups_description()
            .and_then(|d| d.point_in_time_recovery_description())
        {
            Some(p) => {
                let status = p
                    .point_in_time_recovery_status()
                    .map(|s| s.as_str())
                    .unwrap_or("?");
                match p.earliest_restorable_date_time() {
                    Some(d) => format!("{status} · restorable from {}", date(d)),
                    None => status.to_string(),
                }
            }
            None => "?".to_string(),
        },
        Err(e) => format!("unavailable: {:#}", sdk_err(e)),
    };
    row("Point-in-time recovery", pitr);
    row(
        "Created",
        t.creation_date_time().map(date).unwrap_or_default(),
    );

    let mut section = |title: &str, lines: Vec<String>| {
        let _ = writeln!(s, "\n {title}");
        if lines.is_empty() {
            let _ = writeln!(s, "   (none)");
        }
        for l in lines {
            let _ = writeln!(s, "   {l}");
        }
    };
    section(
        "Global secondary indexes",
        t.global_secondary_indexes()
            .iter()
            .map(|g| {
                let mut line = format!(
                    "{} · {} · {} · {} · ~{} items",
                    g.index_name().unwrap_or("?"),
                    keys(g.key_schema()),
                    projection(g.projection()),
                    g.index_status().map(|s| s.as_str()).unwrap_or("?"),
                    g.item_count().unwrap_or(0)
                );
                if let Some(c) = capacity(g.provisioned_throughput()) {
                    line.push_str(&format!(" · {c}"));
                }
                line
            })
            .collect(),
    );
    section(
        "Local secondary indexes",
        t.local_secondary_indexes()
            .iter()
            .map(|l| {
                format!(
                    "{} · {} · {} · ~{} items",
                    l.index_name().unwrap_or("?"),
                    keys(l.key_schema()),
                    projection(l.projection()),
                    l.item_count().unwrap_or(0)
                )
            })
            .collect(),
    );
    section(
        "Tags",
        match tags {
            Ok(tags) => tags
                .into_iter()
                .map(|(k, v)| format!("{k} = {v}"))
                .collect(),
            Err(e) => vec![format!("unavailable: {e:#}")],
        },
    );
    Ok(s)
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

/// Provisioned read/write units; on-demand tables report zeros, which are omitted.
fn capacity(p: Option<&ProvisionedThroughputDescription>) -> Option<String> {
    let p = p?;
    let (r, w) = (
        p.read_capacity_units().unwrap_or(0),
        p.write_capacity_units().unwrap_or(0),
    );
    (r > 0 || w > 0).then(|| format!("read {r} / write {w}"))
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

fn on_off(b: bool) -> String {
    if b { "on" } else { "off" }.to_string()
}

fn date(d: &DateTime) -> String {
    d.fmt(DateTimeFormat::DateTime).unwrap_or_default()
}

fn bytes(n: i64) -> String {
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
}
