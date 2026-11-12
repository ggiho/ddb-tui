//! Private JSON-line connection to the CLI-owned result cursor.
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug)]
pub struct Batch {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub nulls: Vec<Vec<bool>>,
    pub base: usize,
    pub has_more: bool,
    pub can_fetch: bool,
    pub message: String,
    pub mode: String,
}
impl Batch {
    pub fn parse(value: Value) -> Result<Self> {
        ensure!(
            value["protocol"].as_u64() == Some(1),
            "unsupported result protocol"
        );
        if let Some(error) = value["error"].as_str() {
            bail!("{error}");
        }
        let strings = |v: &Value| -> Result<Vec<String>> {
            v.as_array()
                .context("expected result array")?
                .iter()
                .map(|x| {
                    x.as_str()
                        .map(str::to_owned)
                        .context("expected display text")
                })
                .collect()
        };
        let headers = strings(&value["headers"])?;
        let rows: Vec<Vec<String>> = value["rows"]
            .as_array()
            .context("missing rows")?
            .iter()
            .map(strings)
            .collect::<Result<_>>()?;
        let nulls: Vec<Vec<bool>> = value["nulls"]
            .as_array()
            .context("missing null flags")?
            .iter()
            .map(|r| {
                r.as_array()
                    .context("invalid null flags")?
                    .iter()
                    .map(|v| v.as_bool().context("invalid null flag"))
                    .collect()
            })
            .collect::<Result<_>>()?;
        ensure!(rows.len() == nulls.len(), "row/null count mismatch");
        ensure!(
            rows.is_empty() || !headers.is_empty(),
            "rows without columns"
        );
        ensure!(
            rows.iter()
                .zip(&nulls)
                .all(|(r, n)| r.len() == headers.len() && n.len() == headers.len()),
            "column count mismatch"
        );
        Ok(Self {
            headers,
            rows,
            nulls,
            base: value["base"].as_u64().unwrap_or(0) as usize,
            has_more: value["has_more"].as_bool().unwrap_or(false),
            can_fetch: value["can_fetch"].as_bool().unwrap_or(false),
            message: value["message"].as_str().unwrap_or("").into(),
            mode: value["fetch_mode"].as_str().unwrap_or("cached").into(),
        })
    }
}
#[derive(Clone)]
pub struct QueryClient(Arc<Mutex<BufReader<UnixStream>>>);
impl QueryClient {
    pub fn connect(path: &Path) -> Result<Self> {
        let stream = UnixStream::connect(path)?;
        stream.set_read_timeout(Some(Duration::from_secs(60)))?;
        stream.set_write_timeout(Some(Duration::from_secs(10)))?;
        Ok(Self(Arc::new(Mutex::new(BufReader::new(stream)))))
    }
    pub fn request(&self, op: &str) -> Result<Batch> {
        let mut channel = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("result channel unavailable"))?;
        writeln!(channel.get_mut(), "{}", json!({"op":op}))?;
        channel.get_mut().flush()?;
        let mut line = String::new();
        ensure!(
            channel.read_line(&mut line)? > 0,
            "result connection closed"
        );
        Batch::parse(serde_json::from_str(&line)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn typed_null_flags_are_required_and_not_inferred_from_text() {
        let valid = json!({"protocol":1,"headers":["v"],"rows":[["NULL"],["NULL"]],"nulls":[[false],[true]],"base":0,"has_more":false,"can_fetch":false});
        let batch = Batch::parse(valid.clone()).unwrap();
        assert!(!batch.nulls[0][0] && batch.nulls[1][0]);
        let mut bad = valid.clone();
        bad["nulls"] = json!([[true]]);
        assert!(Batch::parse(bad).is_err());
        let mut bad = valid;
        bad["protocol"] = json!(2);
        assert!(Batch::parse(bad).is_err());
        assert!(Batch::parse(json!({"protocol":1,"headers":[],"rows":[[]],"nulls":[[]]})).is_err());
    }
}
