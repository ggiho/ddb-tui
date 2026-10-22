//! Saved operations in NoSQL Workbench's format, stored where Workbench keeps them:
//! `~/.aws/workbench/saved-operations/<connection>/<region>/dynamodb-saved-operation-<name>.json`.
//! Scan, Query and PartiQL statements round-trip; other operation types are listed only.
//!
//! Workbench stores filters as a tree (`logicalOperator`, `negate`, `conditions`,
//! `childExpressions`) of single-attribute conditions. Loading turns that tree into filter text;
//! saving parses the text back and refuses what the tree cannot express (nested paths,
//! attribute-to-attribute comparisons), since Workbench would read those differently.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, anyhow, bail};
use aws_sdk_dynamodb::types::AttributeValue;
use serde_json::{Value, json};

use crate::ddb::{SkOp, TableInfo};
use crate::expr::{self, Mode, Seg, Token};
use crate::form::ReadForm;
use crate::{export, item};

const FILE_PREFIX: &str = "dynamodb-saved-operation-";
const VERSION: &str = "1.0";

pub fn dir(connection: &str, region: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join(".aws/workbench/saved-operations")
            .join(connection)
            .join(region),
    )
}

pub struct Entry {
    pub name: String,
    pub operation: String,
    pub path: PathBuf,
}

/// Saved operations in `dir`, sorted by name. Missing directories list nothing.
pub fn list(dir: &Path) -> Vec<Entry> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries: Vec<Entry> = read
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let file = e.file_name().to_string_lossy().to_string();
            let name = file
                .strip_prefix(FILE_PREFIX)?
                .strip_suffix(".json")?
                .to_string();
            let operation = std::fs::read_to_string(e.path())
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                .and_then(|v| v["operationType"].as_str().map(str::to_string))
                .unwrap_or_else(|| "?".into());
            Some(Entry {
                name,
                operation,
                path: e.path(),
            })
        })
        .collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries
}

/// A saved Scan or Query in terms of the read form.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SavedRead {
    pub table: String,
    pub index: Option<String>,
    pub query: bool,
    pub pk: String,
    pub sk_op: Option<SkOp>,
    pub sk: String,
    pub sk2: String,
    pub filter: String,
    pub projection: String,
    pub consistent: bool,
    pub descending: bool,
}

impl SavedRead {
    /// The form for `table`; an index that no longer exists falls back to the table.
    pub fn into_form(self, table: &TableInfo) -> ReadForm {
        let mut f = ReadForm::default();
        f.query = self.query;
        f.index = table
            .indexes
            .iter()
            .position(|i| i.name == self.index)
            .unwrap_or(0);
        f.pk = self.pk;
        f.op = self
            .sk_op
            .and_then(|op| SkOp::ALL.iter().position(|o| *o == op))
            .map_or(0, |i| i + 1);
        f.sk = self.sk;
        f.sk2 = self.sk2;
        f.filter = self.filter;
        f.projection = self.projection;
        f.consistent = self.consistent;
        f.descending = self.descending;
        f
    }
}

#[derive(Debug, PartialEq)]
pub enum Loaded {
    Read(SavedRead),
    PartiQL(String),
    /// An operation ddb-tui does not run, with the reason.
    Unsupported(String),
}

pub fn load(path: &Path) -> Result<Loaded> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let v: Value = serde_json::from_str(&text).context("saved operation is not valid JSON")?;
    let op = v["operationType"].as_str().unwrap_or_default();
    let f = &v["operationForm"];
    match op {
        "Scan" | "Query" => {
            let query = op == "Query";
            let mut read = SavedRead {
                table: f["tableName"].as_str().unwrap_or_default().to_string(),
                index: v["indexName"].as_str().map(str::to_string),
                query,
                filter: group_text(&f["filterExpression"])?.unwrap_or_default(),
                projection: f["projectedAttributes"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|n| n.as_str())
                            .map(expr::name_text)
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default(),
                consistent: f["consistentRead"].as_bool().unwrap_or(false),
                descending: query && f["scanIndexForward"].as_bool() == Some(false),
                ..SavedRead::default()
            };
            if query {
                let conds = f["keyConditionExpression"]["conditions"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                if let Some(pk) = conds.first() {
                    read.pk = string(&pk["attributeValue"]);
                }
                if let Some(sk) = conds
                    .get(1)
                    .filter(|c| !string(&c["comparisionOperator"]).is_empty())
                {
                    let (op, a, b) = match string(&sk["comparisionOperator"]).as_str() {
                        "BETWEEN" => (
                            SkOp::Between,
                            string(&sk["betweenStart"]),
                            string(&sk["betweenEnd"]),
                        ),
                        "begins_with" => (
                            SkOp::BeginsWith,
                            string(&sk["attributeValue"]),
                            String::new(),
                        ),
                        other => {
                            let op = SkOp::ALL
                                .into_iter()
                                .find(|o| o.label() == other)
                                .ok_or_else(|| anyhow!("unknown sort key operator {other}"))?;
                            (op, string(&sk["attributeValue"]), String::new())
                        }
                    };
                    // Workbench keeps an empty sort key row when none is used.
                    if !a.is_empty() {
                        read.sk_op = Some(op);
                        read.sk = a;
                        read.sk2 = b;
                    }
                }
            }
            Ok(Loaded::Read(read))
        }
        "ExecuteStatement" => {
            let params = f["parametersForm"].as_array().map_or(0, Vec::len);
            if params > 0 {
                return Ok(Loaded::Unsupported(
                    "PartiQL with parameters is not supported yet".into(),
                ));
            }
            Ok(Loaded::PartiQL(string(&f["statement"])))
        }
        "" => bail!("saved operation has no operationType"),
        other => Ok(Loaded::Unsupported(format!(
            "{other} is not supported by ddb-tui"
        ))),
    }
}

fn string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Workbench's filter tree as filter text, or `None` when it has no complete condition.
fn group_text(g: &Value) -> Result<Option<String>> {
    if !g.is_object() {
        return Ok(None);
    }
    let joiner = if g["logicalOperator"] == "Or" {
        " OR "
    } else {
        " AND "
    };
    let mut parts = Vec::new();
    for c in g["conditions"].as_array().into_iter().flatten() {
        if let Some(t) = condition_text(c)? {
            parts.push(t);
        }
    }
    for child in g["childExpressions"].as_array().into_iter().flatten() {
        if let Some(t) = group_text(child)? {
            parts.push(format!("({t})"));
        }
    }
    if parts.is_empty() {
        return Ok(None);
    }
    let text = parts.join(joiner);
    Ok(Some(if g["negate"] == true {
        format!("NOT ({text})")
    } else {
        text
    }))
}

/// One stored condition as filter text: a comparison, `BETWEEN`, `IN`, or one of the
/// condition functions, negated when its `negate` flag is set.
fn condition_text(c: &Value) -> Result<Option<String>> {
    let name = string(&c["attributeName"]);
    if name.is_empty() {
        return Ok(None);
    }
    let n = expr::name_text(&name);
    let ty = string(&c["attributeType"]);
    let value = |raw: &Value, ty: &str| -> Result<String> {
        let v = workbench_value(&string(raw), ty)?;
        expr::literal_text(&v).ok_or_else(|| anyhow!("{ty} values are not supported in filters"))
    };
    let compare = |op: &str, ty: &str| -> Result<Option<String>> {
        Ok(match op {
            "=" | "<>" | "<" | ">" | "<=" | ">=" => {
                Some(format!(" {op} {}", value(&c["attributeValue"], ty)?))
            }
            "BETWEEN" => Some(format!(
                " BETWEEN {} AND {}",
                value(&c["betweenStart"], ty)?,
                value(&c["betweenEnd"], ty)?
            )),
            "IN" => Some(format!(
                " IN ({})",
                string(&c["attributeValue"])
                    .split(',')
                    .map(|raw| value(&Value::String(raw.to_string()), ty))
                    .collect::<Result<Vec<_>>>()?
                    .join(", ")
            )),
            _ => None,
        })
    };
    let op = string(&c["comparisionOperator"]);
    let text = if let Some(rest) = compare(&op, &ty)? {
        format!("{n}{rest}")
    } else {
        match op.as_str() {
            "attribute_exists" | "attribute_not_exists" => format!("{op}({n})"),
            "attribute_type" => format!("attribute_type({n}, \"{ty}\")"),
            "begins_with" | "contains" => {
                format!("{op}({n}, {})", value(&c["attributeValue"], &ty)?)
            }
            "size" => {
                let rest = compare(&string(&c["sizeOperator"]), "N")?
                    .ok_or_else(|| anyhow!("size needs a comparison"))?;
                format!("size({n}){rest}")
            }
            other => bail!("unknown condition operator {other}"),
        }
    };
    Ok(Some(if c["negate"] == true {
        format!("NOT ({text})")
    } else {
        text
    }))
}

/// A stored value: raw text interpreted by its attribute type (sets and lists as JSON).
fn workbench_value(raw: &str, ty: &str) -> Result<AttributeValue> {
    let json_strings = |raw: &str| -> Result<Vec<String>> {
        let v: Vec<Value> =
            serde_json::from_str(raw).with_context(|| format!("invalid {ty} value {raw}"))?;
        Ok(v.iter().map(string).collect())
    };
    Ok(match ty {
        "S" | "" => AttributeValue::S(raw.to_string()),
        "N" => AttributeValue::N(raw.to_string()),
        "BOOL" => AttributeValue::Bool(raw.trim() == "true"),
        "NULL" => AttributeValue::Null(true),
        "SS" => AttributeValue::Ss(json_strings(raw)?),
        "NS" => AttributeValue::Ns(json_strings(raw)?),
        "L" => item::from_json(&json!({ "L": serde_json::from_str::<Value>(raw)? }))?,
        other => bail!("{other} values are not supported in filters"),
    })
}

/// The attribute type and raw text Workbench stores for a value.
fn workbench_raw(v: &AttributeValue) -> Result<(&'static str, String)> {
    Ok(match v {
        AttributeValue::S(s) => ("S", s.clone()),
        AttributeValue::N(n) => ("N", n.clone()),
        AttributeValue::Bool(b) => ("BOOL", b.to_string()),
        AttributeValue::Null(_) => ("NULL", "true".into()),
        AttributeValue::Ss(v) => ("SS", serde_json::to_string(v)?),
        AttributeValue::Ns(v) => ("NS", serde_json::to_string(v)?),
        AttributeValue::L(_) => ("L", item::to_json(v)["L"].to_string()),
        _ => bail!("this value type cannot be saved in Workbench format"),
    })
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// A random-looking UUID for condition ids; Workbench only needs them unique.
fn new_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut x = nanos ^ NEXT_ID.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed);
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let (a, b) = (next(), next());
    format!(
        "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
        a >> 32,
        (a >> 16) & 0xffff,
        a & 0xfff,
        (b >> 48) & 0x3fff | 0x8000,
        b & 0xffff_ffff_ffff
    )
}

/// Filter syntax tree: groups of leaves, where a leaf is a Workbench condition object.
enum Ast {
    Group { or: bool, items: Vec<Ast> },
    Not(Box<Ast>),
    Leaf(Value),
}

struct Parser {
    toks: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.toks.get(self.pos)
    }

    fn next(&mut self) -> Option<Token> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }

    fn eat_keyword(&mut self, k: &str) -> bool {
        let hit = matches!(self.peek(), Some(Token::Keyword(w)) if w == k);
        if hit {
            self.pos += 1;
        }
        hit
    }

    fn expect(&mut self, sym: &str) -> Result<()> {
        match self.next() {
            Some(Token::Sym(s)) if s == sym => Ok(()),
            other => bail!("expected '{sym}', found {other:?}"),
        }
    }

    fn value(&mut self) -> Result<AttributeValue> {
        match self.next() {
            Some(Token::Value(v)) => Ok(v),
            Some(Token::Path(_)) => {
                bail!("comparing two attributes cannot be saved in Workbench format")
            }
            other => bail!("expected a value, found {other:?}"),
        }
    }

    /// Workbench conditions name a single top-level attribute.
    fn name(&mut self) -> Result<String> {
        match self.next() {
            Some(Token::Path(p)) => match p.as_slice() {
                [Seg::Name(n)] => Ok(n.clone()),
                _ => bail!("nested paths cannot be saved in Workbench format"),
            },
            other => bail!("expected an attribute name, found {other:?}"),
        }
    }

    fn or(&mut self) -> Result<Ast> {
        let mut items = vec![self.and()?];
        while self.eat_keyword("OR") {
            items.push(self.and()?);
        }
        Ok(if items.len() == 1 {
            items.pop().expect("one item")
        } else {
            Ast::Group { or: true, items }
        })
    }

    fn and(&mut self) -> Result<Ast> {
        let mut items = vec![self.not()?];
        while self.eat_keyword("AND") {
            items.push(self.not()?);
        }
        Ok(if items.len() == 1 {
            items.pop().expect("one item")
        } else {
            Ast::Group { or: false, items }
        })
    }

    fn not(&mut self) -> Result<Ast> {
        if self.eat_keyword("NOT") {
            return Ok(Ast::Not(Box::new(self.not()?)));
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Ast> {
        match self.peek().cloned() {
            Some(Token::Sym("(")) => {
                self.pos += 1;
                let inner = self.or()?;
                self.expect(")")?;
                Ok(inner)
            }
            Some(Token::Func(f)) => {
                self.pos += 1;
                self.expect("(")?;
                let name = self.name()?;
                let leaf = match f.as_str() {
                    "attribute_exists" | "attribute_not_exists" => {
                        self.expect(")")?;
                        leaf(&name, &f, "", "")
                    }
                    "attribute_type" => {
                        self.expect(",")?;
                        let ty = match self.value()? {
                            AttributeValue::S(t) => t,
                            _ => bail!("attribute_type takes a type name such as \"S\""),
                        };
                        self.expect(")")?;
                        leaf(&name, &f, &ty, "")
                    }
                    "begins_with" | "contains" => {
                        self.expect(",")?;
                        let (ty, raw) = workbench_raw(&self.value()?)?;
                        self.expect(")")?;
                        leaf(&name, &f, ty, &raw)
                    }
                    "size" => {
                        self.expect(")")?;
                        let mut c = leaf(&name, "size", "N", "");
                        self.comparison_into(&mut c, "sizeOperator")?;
                        c
                    }
                    other => bail!("{other} cannot be saved in Workbench format"),
                };
                Ok(Ast::Leaf(leaf))
            }
            Some(Token::Path(_)) => {
                let name = self.name()?;
                let mut c = leaf(&name, "", "", "");
                self.comparison_into(&mut c, "comparisionOperator")?;
                Ok(Ast::Leaf(c))
            }
            other => bail!("unexpected {other:?} in filter"),
        }
    }

    /// `op value`, `BETWEEN a AND b` or `IN (…)` after an attribute or `size(…)`.
    fn comparison_into(&mut self, c: &mut Value, op_field: &str) -> Result<()> {
        let op = match self.next() {
            Some(Token::Sym(s @ ("=" | "<>" | "<" | ">" | "<=" | ">="))) => {
                let (ty, raw) = workbench_raw(&self.value()?)?;
                if op_field == "comparisionOperator" {
                    c["attributeType"] = json!(ty);
                }
                c["attributeValue"] = json!(raw);
                s.to_string()
            }
            Some(Token::Keyword(k)) if k == "BETWEEN" => {
                let (ty, low) = workbench_raw(&self.value()?)?;
                if !self.eat_keyword("AND") {
                    bail!("BETWEEN needs AND");
                }
                let (ty2, high) = workbench_raw(&self.value()?)?;
                if ty != ty2 {
                    bail!("BETWEEN bounds must have the same type");
                }
                if op_field == "comparisionOperator" {
                    c["attributeType"] = json!(ty);
                }
                c["betweenStart"] = json!(low);
                c["betweenEnd"] = json!(high);
                "BETWEEN".into()
            }
            Some(Token::Keyword(k)) if k == "IN" && op_field == "comparisionOperator" => {
                self.expect("(")?;
                let mut raws = Vec::new();
                let mut types = Vec::new();
                loop {
                    let (ty, raw) = workbench_raw(&self.value()?)?;
                    if raw.contains(',') {
                        bail!("IN values containing commas cannot be saved in Workbench format");
                    }
                    types.push(ty);
                    raws.push(raw);
                    match self.next() {
                        Some(Token::Sym(",")) => continue,
                        Some(Token::Sym(")")) => break,
                        other => bail!("expected ',' or ')', found {other:?}"),
                    }
                }
                if types.windows(2).any(|w| w[0] != w[1]) {
                    bail!("IN values must have the same type");
                }
                c["attributeType"] = json!(types[0]);
                c["attributeValue"] = json!(raws.join(","));
                "IN".into()
            }
            other => bail!("expected a comparison, found {other:?}"),
        };
        c[op_field] = json!(op);
        Ok(())
    }
}

fn leaf(name: &str, op: &str, ty: &str, value: &str) -> Value {
    json!({
        "id": new_id(),
        "attributeName": name,
        "comparisionOperator": op,
        "attributeType": ty,
        "attributeValue": value,
        "negate": false,
    })
}

fn group_json(or: bool, negate: bool, items: Vec<Ast>) -> Value {
    let mut conditions = Vec::new();
    let mut children = Vec::new();
    for item in items {
        push_item(item, &mut conditions, &mut children);
    }
    json!({
        "id": new_id(),
        "logicalOperator": if or { "Or" } else { "And" },
        "negate": negate,
        "conditions": conditions,
        "childExpressions": children,
    })
}

/// Leaves become conditions and groups child expressions; a double negation cancels out.
fn push_item(item: Ast, conditions: &mut Vec<Value>, children: &mut Vec<Value>) {
    match item {
        Ast::Leaf(c) => conditions.push(c),
        Ast::Group { or, items } => children.push(group_json(or, false, items)),
        Ast::Not(inner) => match *inner {
            Ast::Leaf(mut c) => {
                c["negate"] = json!(true);
                conditions.push(c);
            }
            Ast::Group { or, items } => children.push(group_json(or, true, items)),
            Ast::Not(twice) => push_item(*twice, conditions, children),
        },
    }
}

/// Filter text as Workbench's condition tree.
pub fn filter_tree(text: &str) -> Result<Value> {
    let mut p = Parser {
        toks: expr::tokenize(text, Mode::Condition)?,
        pos: 0,
    };
    let ast = p.or()?;
    if p.pos < p.toks.len() {
        bail!("unexpected {:?} after the filter", p.toks[p.pos]);
    }
    let mut tree = match ast {
        Ast::Group { or, items } => group_json(or, false, items),
        Ast::Not(inner) => match *inner {
            Ast::Group { or, items } => group_json(or, true, items),
            other => group_json(false, false, vec![Ast::Not(Box::new(other))]),
        },
        leaf => group_json(false, false, vec![leaf]),
    };
    tree["id"] = json!("");
    Ok(tree)
}

fn empty_filter(with_row: bool) -> Value {
    json!({
        "id": "",
        "logicalOperator": "And",
        "negate": false,
        "conditions": if with_row { vec![leaf("", "=", "", "")] } else { Vec::new() },
        "childExpressions": [],
    })
}

fn file_path(dir: &Path, name: &str) -> Result<PathBuf> {
    let name = name.trim();
    if name.is_empty() || name.contains(['/', '\\']) {
        bail!("a saved operation name cannot be empty or contain / or \\");
    }
    Ok(dir.join(format!("{FILE_PREFIX}{name}.json")))
}

fn write(dir: &Path, name: &str, doc: &Value) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let path = file_path(dir, name)?;
    if path.exists() {
        bail!("a saved operation named {} already exists", name.trim());
    }
    export::write_new_private(&path, &serde_json::to_string(doc)?)?;
    Ok(path)
}

/// The Workbench document for a Scan or Query from the read form.
pub fn read_document(name: &str, table: &TableInfo, form: &ReadForm) -> Result<Value> {
    let filter = match form.filter.trim() {
        "" => empty_filter(!form.query),
        text => filter_tree(text).context("Filter")?,
    };
    let projected = match form.projection.trim() {
        "" => Value::Null,
        text => json!(
            expr::projection_paths(text)?
                .into_iter()
                .map(|p| match p.as_slice() {
                    [Seg::Name(n)] => Ok(n.clone()),
                    _ => Err(anyhow!(
                        "Projection: nested paths cannot be saved in Workbench format"
                    )),
                })
                .collect::<Result<Vec<_>>>()?
        ),
    };
    let idx = form.selected_index(table);
    let mut op_form = json!({
        "tableName": table.name,
        "generatedCodes": null,
        "projectedAttributes": projected,
        "filterExpression": filter,
        "consistentRead": form.consistent,
        "limit": null,
        "exclusiveStartKey": null,
        "operationType": if form.query { "Query" } else { "Scan" },
    });
    if form.query {
        anyhow::ensure!(!form.pk.is_empty(), "Enter a value for {}", idx.pk.name);
        let mut conditions = vec![leaf(&idx.pk.name, "=", &idx.pk.ty, &form.pk)];
        if let (Some(op), Some(sk)) = (form.sk_op(), &idx.sk) {
            let mut c = match op {
                SkOp::Between => leaf(&sk.name, "BETWEEN", &sk.ty, ""),
                SkOp::BeginsWith => leaf(&sk.name, "begins_with", &sk.ty, &form.sk),
                op => leaf(&sk.name, op.label(), &sk.ty, &form.sk),
            };
            if op == SkOp::Between {
                c["betweenStart"] = json!(form.sk);
                c["betweenEnd"] = json!(form.sk2);
            }
            conditions.push(c);
        }
        op_form["keyConditionExpression"] = json!({
            "id": "",
            "logicalOperator": "And",
            "negate": false,
            "conditions": conditions,
            "childExpressions": [],
        });
        op_form["scanIndexForward"] = json!(!form.descending);
    } else {
        op_form["segment"] = Value::Null;
        op_form["totalSegments"] = Value::Null;
    }
    let mut doc = json!({
        "name": name.trim(),
        "operationType": if form.query { "Query" } else { "Scan" },
        "metadata": { "version": VERSION },
        "operationForm": op_form,
    });
    if let Some(index) = &idx.name {
        doc["indexName"] = json!(index);
        doc["indexType"] = json!(if idx.global { "Gsi" } else { "Lsi" });
    }
    Ok(doc)
}

pub fn save_read(dir: &Path, name: &str, table: &TableInfo, form: &ReadForm) -> Result<PathBuf> {
    write(dir, name, &read_document(name, table, form)?)
}

pub fn save_partiql(dir: &Path, name: &str, statement: &str) -> Result<PathBuf> {
    let doc = json!({
        "name": name.trim(),
        "operationType": "ExecuteStatement",
        "metadata": { "version": VERSION },
        "operationForm": { "statement": statement, "parametersForm": [], "consistentRead": false },
    });
    write(dir, name, &doc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ddb::{Index, Key};

    fn table() -> TableInfo {
        let key = |n: &str, t: &str| Key {
            name: n.into(),
            ty: t.into(),
        };
        TableInfo {
            name: "Demo".into(),
            indexes: vec![
                Index {
                    name: None,
                    pk: key("pk", "S"),
                    sk: Some(key("sk", "N")),
                    global: false,
                },
                Index {
                    name: Some("by_status".into()),
                    pk: key("status", "S"),
                    sk: Some(key("sk", "N")),
                    global: true,
                },
            ],
            item_count: None,
            size_bytes: None,
        }
    }

    fn without_ids(v: &Value) -> Value {
        match v {
            Value::Object(m) => Value::Object(
                m.iter()
                    .filter(|(k, _)| *k != "id")
                    .map(|(k, v)| (k.clone(), without_ids(v)))
                    .collect(),
            ),
            Value::Array(a) => Value::Array(a.iter().map(without_ids).collect()),
            other => other.clone(),
        }
    }

    /// Text → tree → text → tree reaches the same tree: loading what we saved changes nothing.
    #[test]
    fn filters_round_trip_through_the_workbench_tree() {
        let cases = [
            "status = \"open\"",
            "a = 1 AND b <> 'x' OR NOT c BETWEEN 1 AND 5",
            "attribute_exists(note) AND NOT (x IN (1, 2) OR size(tags) >= 2)",
            "attribute_type(v, 'SS') AND begins_with(name, \"Al\") AND contains(tags, \"a\")",
            "flag = true AND gone = null AND s IN (\"a\", \"b\")",
            "tags = <<\"a\", \"b\">> AND size(name) BETWEEN 1 AND 3 AND l = [1, \"x\"]",
            "`odd name` = \"q\" AND NOT NOT a = 1",
        ];
        for text in cases {
            let tree = filter_tree(text).unwrap();
            let back = group_text(&tree).unwrap().unwrap();
            let again = filter_tree(&back).unwrap_or_else(|e| panic!("{text} -> {back}: {e}"));
            assert_eq!(without_ids(&tree), without_ids(&again), "{text} -> {back}");
        }
    }

    #[test]
    fn precedence_and_negation_map_to_groups() {
        let tree = filter_tree("a = 1 AND b = 2 OR NOT (c = 3 AND d = 4)").unwrap();
        assert_eq!(tree["logicalOperator"], "Or");
        assert_eq!(tree["conditions"].as_array().unwrap().len(), 0);
        let children = tree["childExpressions"].as_array().unwrap();
        assert_eq!(children.len(), 2);
        assert_eq!(children[0]["logicalOperator"], "And");
        assert_eq!(children[1]["negate"], true);
        assert_eq!(
            group_text(&tree).unwrap().unwrap(),
            "(a = 1 AND b = 2) OR (NOT (c = 3 AND d = 4))"
        );
    }

    #[test]
    fn refuses_what_workbench_cannot_express() {
        assert!(
            filter_tree("profile.city = 'x'")
                .unwrap_err()
                .to_string()
                .contains("nested")
        );
        assert!(
            filter_tree("a = b")
                .unwrap_err()
                .to_string()
                .contains("two attributes")
        );
        assert!(filter_tree("a IN (\"x,y\")").is_err());
    }

    #[test]
    fn query_saves_and_loads_like_workbench() {
        let t = table();
        let mut f = ReadForm::default();
        f.index = 1;
        f.pk = "closed".into();
        f.op = 7; // between
        f.sk = "10".into();
        f.sk2 = "20".into();
        f.filter = "amount > 40".into();
        f.projection = "pk, sk, amount".into();
        f.descending = true;
        let doc = read_document("closed big", &t, &f).unwrap();
        assert_eq!(doc["indexType"], "Gsi");
        let key = &doc["operationForm"]["keyConditionExpression"]["conditions"];
        assert_eq!(key[0]["attributeName"], "status");
        assert_eq!(key[1]["comparisionOperator"], "BETWEEN");
        assert_eq!(key[1]["betweenEnd"], "20");
        assert_eq!(
            doc["operationForm"]["projectedAttributes"],
            json!(["pk", "sk", "amount"])
        );

        let dir = std::env::temp_dir().join(format!("ddb-tui-saved-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = save_read(&dir, "closed big", &t, &f).unwrap();
        assert!(
            save_read(&dir, "closed big", &t, &f).is_err(),
            "never overwrites"
        );
        let Loaded::Read(read) = load(&path).unwrap() else {
            panic!("expected a read")
        };
        let back = read.into_form(&t);
        assert_eq!(
            (
                back.query,
                back.index,
                back.pk.as_str(),
                back.op,
                back.sk.as_str(),
                back.sk2.as_str()
            ),
            (true, 1, "closed", 7, "10", "20")
        );
        assert_eq!(
            (
                back.filter.as_str(),
                back.projection.as_str(),
                back.descending
            ),
            ("amount > 40", "pk, sk, amount", true)
        );

        save_partiql(&dir, "stmt", "SELECT * FROM \"Demo\"").unwrap();
        let names: Vec<_> = list(&dir)
            .into_iter()
            .map(|e| (e.name, e.operation))
            .collect();
        assert_eq!(
            names,
            [
                ("closed big".to_string(), "Query".to_string()),
                ("stmt".into(), "ExecuteStatement".into())
            ]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A document shaped like the ones Workbench writes, including an empty scan filter row.
    #[test]
    fn loads_workbench_written_scans() {
        let dir = std::env::temp_dir().join(format!("ddb-tui-saved-wb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{FILE_PREFIX}wb.json"));
        let doc = json!({
            "name": "wb", "operationType": "Scan", "metadata": {"version": "1.0"},
            "operationForm": {
                "tableName": "Demo", "projectedAttributes": null, "consistentRead": true,
                "filterExpression": {
                    "id": "", "logicalOperator": "Or", "negate": false,
                    "conditions": [
                        {"id": "1", "attributeName": "status", "comparisionOperator": "=", "attributeType": "S", "attributeValue": "closed", "negate": false},
                        {"id": "2", "attributeName": "", "comparisionOperator": "=", "attributeType": "", "attributeValue": "", "negate": false}
                    ],
                    "childExpressions": [{
                        "id": "3", "logicalOperator": "And", "negate": true,
                        "conditions": [
                            {"id": "4", "attributeName": "tags", "comparisionOperator": "size", "sizeOperator": ">", "attributeType": "N", "attributeValue": "2", "negate": false},
                            {"id": "5", "attributeName": "n", "comparisionOperator": "IN", "attributeType": "N", "attributeValue": "1,2", "negate": true}
                        ],
                        "childExpressions": []
                    }]
                }
            }
        });
        std::fs::write(&path, doc.to_string()).unwrap();
        let Loaded::Read(read) = load(&path).unwrap() else {
            panic!("expected a read")
        };
        assert_eq!(
            read.filter,
            "status = \"closed\" OR (NOT (size(tags) > 2 AND NOT (n IN (1, 2))))"
        );
        assert!(read.consistent && !read.query);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
