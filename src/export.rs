//! Writing items to files: CSV for spreadsheets, DynamoDB JSON for lossless round trips.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::item::{self, Item};

/// CSV with the given column order; complex values are written as plain JSON.
pub fn to_csv(columns: &[String], items: &[Item]) -> String {
    let mut out = String::new();
    let row = |fields: Vec<String>| {
        fields
            .iter()
            .map(|f| csv_field(f))
            .collect::<Vec<_>>()
            .join(",")
            + "\r\n"
    };
    out.push_str(&row(columns.to_vec()));
    for it in items {
        out.push_str(&row(columns
            .iter()
            .map(|c| it.get(c).map(item::text).unwrap_or_default())
            .collect()));
    }
    out
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) || s.starts_with(' ') || s.ends_with(' ') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// A JSON array of items in DynamoDB JSON.
pub fn to_json(items: &[Item]) -> Result<String> {
    let values: Vec<_> = items.iter().map(item::item_to_json).collect();
    Ok(serde_json::to_string_pretty(&values)?)
}

/// Writes `.csv` or `.json` by extension. Existing files are never overwritten.
pub fn export(path: &Path, columns: &[String], items: &[Item]) -> Result<()> {
    let text = match path.extension().and_then(|e| e.to_str()) {
        Some("csv") => to_csv(columns, items),
        Some("json") => to_json(items)?,
        _ => bail!("use a .csv or .json file name"),
    };
    write_new_private(path, &text)
}

/// Creates the file readable only by the current user, since it holds table data.
pub fn write_new_private(path: &Path, text: &str) -> Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("cannot create {}", path.display()))?;
    f.write_all(text.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_dynamodb::types::AttributeValue;

    fn sample() -> Vec<Item> {
        vec![
            Item::from([
                ("pk".to_string(), AttributeValue::S("a,b".into())),
                ("n".to_string(), AttributeValue::N("1.50".into())),
                (
                    "m".to_string(),
                    AttributeValue::M(Item::from([(
                        "x".to_string(),
                        AttributeValue::S("say \"hi\"".into()),
                    )])),
                ),
            ]),
            Item::from([("pk".to_string(), AttributeValue::S("line1\nline2".into()))]),
        ]
    }

    #[test]
    fn csv_quotes_only_when_needed() {
        let cols = ["pk", "n", "m"].map(String::from);
        let csv = to_csv(&cols, &sample());
        assert_eq!(
            csv,
            "pk,n,m\r\n\"a,b\",1.50,\"{\"\"x\"\":\"\"say \\\"\"hi\\\"\"\"\"}\"\r\n\"line1\nline2\",,\r\n"
        );
    }

    #[test]
    fn json_export_round_trips() {
        let items = sample();
        let text = to_json(&items).unwrap();
        let back: Vec<Item> = serde_json::from_str::<Vec<serde_json::Value>>(&text)
            .unwrap()
            .iter()
            .map(|v| item::item_from_json(v).unwrap())
            .collect();
        assert_eq!(back, items);
    }

    #[test]
    fn never_overwrites_and_restricts_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("ddb-tui-export-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.csv");
        let _ = std::fs::remove_file(&path);
        export(&path, &["pk".to_string()], &sample()).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(
            export(&path, &["pk".to_string()], &sample()).is_err(),
            "second write must fail"
        );
        assert!(export(&dir.join("out.txt"), &[], &[]).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
