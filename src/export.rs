//! Writing items to files: CSV for spreadsheets, DynamoDB JSON for lossless round trips, and
//! JSON Lines for streaming tools. A page is written at once; a whole result set is written as
//! it is read, so it never has to fit in memory.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};
use aws_sdk_dynamodb::Client;

use crate::ddb::{self, ReadRequest};
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

/// One item per line in DynamoDB JSON.
pub fn to_jsonl(items: &[Item]) -> String {
    items
        .iter()
        .map(|i| format!("{}\n", item::item_to_json(i)))
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    Csv,
    Json,
    JsonLines,
}

fn format_of(path: &Path) -> Result<Format> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("csv") => Ok(Format::Csv),
        Some("json") => Ok(Format::Json),
        Some("jsonl") => Ok(Format::JsonLines),
        _ => bail!("use a .csv, .json or .jsonl file name"),
    }
}

/// Fails unless the name has a supported extension and is not taken yet.
pub fn check_name(path: &Path) -> Result<()> {
    format_of(path)?;
    if path.exists() {
        bail!("{} already exists", path.display());
    }
    Ok(())
}

/// Writes `.csv`, `.json` or `.jsonl` by extension. Existing files are never overwritten.
pub fn export(path: &Path, columns: &[String], items: &[Item]) -> Result<()> {
    let text = match format_of(path)? {
        Format::Csv => to_csv(columns, items),
        Format::Json => to_json(items)?,
        Format::JsonLines => to_jsonl(items),
    };
    write_new_private(path, &text)
}

/// Creates the file readable only by the current user, since it holds table data.
pub fn write_new_private(path: &Path, text: &str) -> Result<()> {
    create_private(path)?.write_all(text.as_bytes())?;
    Ok(())
}

fn create_private(path: &Path) -> Result<File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("cannot create {}", path.display()))
}

/// Progress of a whole-result export, reported after every page.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Progress {
    pub items: usize,
    /// DynamoDB requests made; each reads up to 1 MB.
    pub requests: usize,
    pub consumed: f64,
}

/// How a whole-result export ended.
#[derive(Debug, PartialEq)]
pub struct Summary {
    pub progress: Progress,
    /// Stopped on request; the partial file was removed.
    pub cancelled: bool,
}

/// Items read per request while exporting; DynamoDB still stops each call at 1 MB.
const EXPORT_PAGE: usize = 1000;

/// Reads every page of `req` and writes it to `path` as it arrives. `keys` are the columns a
/// CSV starts with. Stops between requests once `cancel` is set, removing what was written; a
/// failure removes it too.
pub async fn export_all(
    client: &Client,
    req: &ReadRequest,
    path: &Path,
    keys: &[String],
    cancel: &Arc<AtomicBool>,
    progress: impl Fn(Progress),
) -> Result<Summary> {
    let mut writer = StreamWriter::create(path)?;
    let mut done = Progress::default();
    let mut cursor = None;
    let result: Result<bool> = async {
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Ok(true);
            }
            let page = ddb::fetch_call(client, req, cursor, EXPORT_PAGE).await?;
            writer.write(&page.items)?;
            done.items += page.items.len();
            done.requests += 1;
            done.consumed += page.consumed.unwrap_or(0.0);
            progress(done);
            cursor = page.next;
            if cursor.is_none() {
                return Ok(false);
            }
        }
    }
    .await;
    match result {
        Ok(false) => {
            writer.finish(keys)?;
            Ok(Summary {
                progress: done,
                cancelled: false,
            })
        }
        Ok(true) => {
            writer.abort();
            Ok(Summary {
                progress: done,
                cancelled: true,
            })
        }
        Err(e) => {
            writer.abort();
            Err(e)
        }
    }
}

/// Writes items as they come. JSON and JSON Lines go straight to the file; a CSV's columns
/// are known only at the end, so its rows wait in a private temporary file beside it.
pub struct StreamWriter {
    path: PathBuf,
    format: Format,
    out: BufWriter<File>,
    temp: Option<(PathBuf, BufWriter<File>)>,
    columns: BTreeSet<String>,
    written: usize,
}

impl StreamWriter {
    /// Creates the file at once, so a name that is taken fails before anything is read.
    pub fn create(path: &Path) -> Result<Self> {
        let format = format_of(path)?;
        let out = BufWriter::new(create_private(path)?);
        let temp = if format == Format::Csv {
            let temp_path = path.with_extension("csv.partial");
            match create_private(&temp_path) {
                Ok(f) => Some((temp_path, BufWriter::new(f))),
                Err(e) => {
                    let _ = std::fs::remove_file(path);
                    return Err(e);
                }
            }
        } else {
            None
        };
        let mut writer = Self {
            path: path.to_path_buf(),
            format,
            out,
            temp,
            columns: BTreeSet::new(),
            written: 0,
        };
        if format == Format::Json {
            writer.out.write_all(b"[")?;
        }
        Ok(writer)
    }

    pub fn write(&mut self, items: &[Item]) -> Result<()> {
        for it in items {
            let line = item::item_to_json(it).to_string();
            match (self.format, &mut self.temp) {
                (Format::Csv, Some((_, temp))) => {
                    self.columns.extend(it.keys().cloned());
                    writeln!(temp, "{line}")?;
                }
                (Format::Json, _) => {
                    let sep = if self.written == 0 { "\n  " } else { ",\n  " };
                    write!(self.out, "{sep}{line}")?;
                }
                _ => writeln!(self.out, "{line}")?,
            }
            self.written += 1;
        }
        Ok(())
    }

    /// Completes the file: closes the JSON array, or writes the CSV with `keys` first and the
    /// other columns in name order.
    pub fn finish(mut self, keys: &[String]) -> Result<()> {
        match self.format {
            Format::Json => self.out.write_all(b"\n]\n")?,
            Format::Csv => {
                let (temp_path, mut temp) = self.temp.take().expect("CSV has a temp file");
                temp.flush()?;
                drop(temp);
                let mut columns: Vec<String> = keys
                    .iter()
                    .filter(|k| self.columns.contains(*k))
                    .cloned()
                    .collect();
                columns.extend(self.columns.iter().filter(|c| !keys.contains(c)).cloned());
                let header = to_csv(&columns, &[]);
                self.out.write_all(header.as_bytes())?;
                for line in BufReader::new(File::open(&temp_path)?).lines() {
                    let value: serde_json::Value = serde_json::from_str(&line?)?;
                    let row = to_csv(&columns, &[item::item_from_json(&value)?]);
                    // Every chunk repeats the header; keep only the row.
                    let row = &row[header.len()..];
                    self.out.write_all(row.as_bytes())?;
                }
                std::fs::remove_file(&temp_path)?;
            }
            Format::JsonLines => {}
        }
        self.out.flush()?;
        Ok(())
    }

    /// Removes everything written so far.
    pub fn abort(mut self) {
        if let Some((temp_path, _)) = self.temp.take() {
            let _ = std::fs::remove_file(temp_path);
        }
        let _ = std::fs::remove_file(&self.path);
    }
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
    fn streamed_files_match_the_page_export() {
        let dir = std::env::temp_dir().join(format!("ddb-tui-stream-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let items = sample();
        let keys = ["pk".to_string()];
        for name in ["all.csv", "all.json", "all.jsonl"] {
            let path = dir.join(name);
            let _ = std::fs::remove_file(&path);
            let mut w = StreamWriter::create(&path).unwrap();
            w.write(&items[..1]).unwrap();
            w.write(&items[1..]).unwrap();
            w.finish(&keys).unwrap();
            let text = std::fs::read_to_string(&path).unwrap();
            match name {
                "all.csv" => assert_eq!(
                    text,
                    to_csv(&["pk", "m", "n"].map(String::from), &items),
                    "keys first, then by name"
                ),
                "all.json" => {
                    let back: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap();
                    assert_eq!(back.len(), 2);
                }
                _ => assert_eq!(text, to_jsonl(&items)),
            }
            assert!(!dir.join("all.csv.partial").exists(), "temp file removed");
        }
        let aborted = dir.join("gone.csv");
        let mut w = StreamWriter::create(&aborted).unwrap();
        w.write(&items).unwrap();
        w.abort();
        assert!(!aborted.exists() && !dir.join("gone.csv.partial").exists());
        assert!(
            StreamWriter::create(&dir.join("all.csv")).is_err(),
            "never overwrites"
        );
        std::fs::remove_dir_all(&dir).unwrap();
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
