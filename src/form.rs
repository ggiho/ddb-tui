//! The Scan/Query form: key condition, filter, projection and read options.

use anyhow::{Context, Result};

use crate::ddb::{Index, ReadOptions, ReadRequest, SkCond, SkOp, TableInfo};
use crate::expr::Placeholders;
use crate::item;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Mode,
    Index,
    Pk,
    Op,
    Sk,
    Sk2,
    Filter,
    Projection,
    Consistent,
    Order,
}

/// `op` 0 means no sort key condition, otherwise `SkOp::ALL[op - 1]`.
#[derive(Clone)]
pub struct ReadForm {
    pub query: bool,
    pub index: usize,
    pub pk: String,
    pub op: usize,
    pub sk: String,
    pub sk2: String,
    pub filter: String,
    pub projection: String,
    pub consistent: bool,
    pub descending: bool,
    field: Field,
}

impl Default for ReadForm {
    /// Starts as a Query focused on the partition key value.
    fn default() -> Self {
        Self {
            query: true,
            index: 0,
            pk: String::new(),
            op: 0,
            sk: String::new(),
            sk2: String::new(),
            filter: String::new(),
            projection: String::new(),
            consistent: false,
            descending: false,
            field: Field::Pk,
        }
    }
}

impl ReadForm {
    pub fn selected_index<'a>(&self, table: &'a TableInfo) -> &'a Index {
        &table.indexes[self.index.min(table.indexes.len() - 1)]
    }

    pub fn fields(&self, table: &TableInfo) -> Vec<Field> {
        let mut fields = vec![Field::Mode, Field::Index];
        if self.query {
            fields.push(Field::Pk);
            if self.selected_index(table).sk.is_some() {
                fields.push(Field::Op);
                if self.op > 0 {
                    fields.push(Field::Sk);
                }
                if self.sk_op() == Some(SkOp::Between) {
                    fields.push(Field::Sk2);
                }
            }
        }
        fields.extend([Field::Filter, Field::Projection, Field::Consistent]);
        if self.query {
            fields.push(Field::Order);
        }
        fields
    }

    /// The focused field; falls back to the first text field when the focused one is hidden.
    pub fn active(&self, table: &TableInfo) -> Field {
        if self.fields(table).contains(&self.field) {
            self.field
        } else if self.query {
            Field::Pk
        } else {
            Field::Filter
        }
    }

    pub fn move_focus(&mut self, table: &TableInfo, delta: isize) {
        let fields = self.fields(table);
        let at = fields
            .iter()
            .position(|f| *f == self.active(table))
            .unwrap_or(0);
        self.field = fields[cycle(at, fields.len(), delta)];
    }

    /// Left/Right on a choice field.
    pub fn change(&mut self, table: &TableInfo, delta: isize) {
        match self.active(table) {
            Field::Mode => self.query = !self.query,
            Field::Index => {
                self.index = cycle(self.index, table.indexes.len(), delta);
                self.op = 0;
            }
            Field::Op => self.op = cycle(self.op, SkOp::ALL.len() + 1, delta),
            Field::Consistent => self.consistent = !self.consistent,
            Field::Order => self.descending = !self.descending,
            _ => {}
        }
    }

    pub fn text_mut(&mut self, table: &TableInfo) -> Option<&mut String> {
        match self.active(table) {
            Field::Pk => Some(&mut self.pk),
            Field::Sk => Some(&mut self.sk),
            Field::Sk2 => Some(&mut self.sk2),
            Field::Filter => Some(&mut self.filter),
            Field::Projection => Some(&mut self.projection),
            _ => None,
        }
    }

    pub fn sk_op(&self) -> Option<SkOp> {
        self.op.checked_sub(1).map(|i| SkOp::ALL[i])
    }

    pub fn request(&self, table: &TableInfo) -> Result<ReadRequest> {
        let idx = self.selected_index(table);
        let mut ph = Placeholders::default();
        let filter = non_blank(&self.filter)
            .map(|t| ph.condition(t))
            .transpose()
            .context("Filter")?;
        let projection = non_blank(&self.projection)
            .map(|t| ph.projection(t))
            .transpose()
            .context("Projection")?;
        let opts = ReadOptions {
            filter,
            projection,
            names: ph.names,
            values: ph.values,
            consistent: self.consistent,
            descending: self.query && self.descending,
            summary: self.summary(),
        };
        if !self.query {
            return Ok(ReadRequest::Scan {
                table: table.name.clone(),
                index: idx.name.clone(),
                opts,
            });
        }

        anyhow::ensure!(!self.pk.is_empty(), "Enter a value for {}", idx.pk.name);
        let pk = (idx.pk.name.clone(), item::scalar(&idx.pk.ty, &self.pk)?);
        let sk = match (self.sk_op(), &idx.sk) {
            (Some(op), Some(k)) => {
                let value2 = if op == SkOp::Between {
                    Some(item::scalar(&k.ty, &self.sk2)?)
                } else {
                    None
                };
                Some((
                    k.name.clone(),
                    SkCond {
                        op,
                        value: item::scalar(&k.ty, &self.sk)?,
                        value2,
                    },
                ))
            }
            _ => None,
        };
        Ok(ReadRequest::Query {
            table: table.name.clone(),
            index: idx.name.clone(),
            pk,
            sk,
            opts,
        })
    }

    fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(f) = non_blank(&self.filter) {
            parts.push(format!("filter {f}"));
        }
        if let Some(p) = non_blank(&self.projection) {
            parts.push(format!("select {p}"));
        }
        if self.consistent {
            parts.push("consistent".into());
        }
        if self.query && self.descending {
            parts.push("descending".into());
        }
        parts.join(" · ")
    }
}

fn non_blank(s: &str) -> Option<&str> {
    let s = s.trim();
    (!s.is_empty()).then_some(s)
}

pub fn cycle(i: usize, n: usize, delta: isize) -> usize {
    (i as isize + delta).rem_euclid(n as isize) as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ddb::Key;
    use aws_sdk_dynamodb::types::AttributeValue;

    fn table() -> TableInfo {
        let key = |n: &str, t: &str| Key {
            name: n.into(),
            ty: t.into(),
        };
        TableInfo {
            name: "t".into(),
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
                    sk: None,
                    global: true,
                },
            ],
            item_count: None,
        }
    }

    #[test]
    fn scan_mode_hides_key_fields() {
        let t = table();
        let mut f = ReadForm::default();
        assert_eq!(f.active(&t), Field::Pk);
        f.query = false;
        assert_eq!(
            f.fields(&t),
            [
                Field::Mode,
                Field::Index,
                Field::Filter,
                Field::Projection,
                Field::Consistent
            ]
        );
        assert_eq!(
            f.active(&t),
            Field::Filter,
            "focus falls back to the filter"
        );
    }

    #[test]
    fn builds_a_scan_with_filter_and_projection() {
        let t = table();
        let f = ReadForm {
            query: false,
            index: 1,
            filter: "status = 'open' and size(tags) > 1".into(),
            projection: "pk, status".into(),
            consistent: true,
            ..ReadForm::default()
        };
        let ReadRequest::Scan { index, opts, .. } = f.request(&t).unwrap() else {
            panic!("expected a scan")
        };
        assert_eq!(index.as_deref(), Some("by_status"));
        assert_eq!(
            opts.filter.as_deref(),
            Some("#n0 = :v0 AND size(#n1) > :v1")
        );
        assert_eq!(opts.projection.as_deref(), Some("#n2, #n0"));
        assert!(opts.consistent);
        assert_eq!(
            opts.summary,
            "filter status = 'open' and size(tags) > 1 · select pk, status · consistent"
        );
    }

    #[test]
    fn query_keeps_key_condition_separate_from_filter_placeholders() {
        let t = table();
        let f = ReadForm {
            pk: "a".into(),
            op: 1,
            sk: "5".into(),
            filter: "x = 1".into(),
            descending: true,
            ..ReadForm::default()
        };
        let ReadRequest::Query { pk, sk, opts, .. } = f.request(&t).unwrap() else {
            panic!("expected a query")
        };
        assert_eq!(pk, ("pk".to_string(), AttributeValue::S("a".into())));
        assert_eq!(sk.unwrap().1.value, AttributeValue::N("5".into()));
        assert_eq!(opts.names["#n0"], "x");
        assert!(opts.descending);
    }

    #[test]
    fn filter_errors_name_the_field() {
        let t = table();
        let f = ReadForm {
            query: false,
            filter: "a = \"x".into(),
            ..ReadForm::default()
        };
        let err = format!("{:#}", f.request(&t).unwrap_err());
        assert!(err.starts_with("Filter: unterminated"), "{err}");
    }

    #[test]
    fn focus_cycles_through_visible_fields() {
        let t = table();
        let mut f = ReadForm::default();
        f.move_focus(&t, -1);
        assert_eq!(f.active(&t), Field::Index);
        f.change(&t, 1);
        assert_eq!(f.selected_index(&t).name.as_deref(), Some("by_status"));
        assert!(
            !f.fields(&t).contains(&Field::Op),
            "by_status has no sort key"
        );
    }
}
