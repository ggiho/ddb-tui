//! What an UpdateItem would do to an item, worked out locally for the confirmation. SET with
//! values, `+`/`-`, `if_not_exists` and `list_append`, REMOVE, and ADD/DELETE on numbers and
//! sets are computed; anything else, such as list indexes, shows the current value with an
//! unknown result. The expression is the placeholder form the update popup builds.

use std::collections::HashMap;

use aws_sdk_dynamodb::types::AttributeValue;

use crate::item::Item;

/// One attribute the update touches.
#[derive(Debug, PartialEq)]
pub struct Change {
    /// The attribute path with real names, such as `profile.city`.
    pub path: String,
    pub before: Option<AttributeValue>,
    /// `Some(None)` when removed; `None` when beyond this preview.
    pub after: Option<Option<AttributeValue>>,
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Word(String),
    Name(String),
    Value(String),
    Index(usize),
    Punct(char),
}

fn tokenize(expr: &str) -> Option<Vec<Token>> {
    let chars: Vec<char> = expr.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    let word = |i: &mut usize| {
        let start = *i;
        while *i < chars.len() && (chars[*i].is_ascii_alphanumeric() || chars[*i] == '_') {
            *i += 1;
        }
        chars[start..*i].iter().collect::<String>()
    };
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '#' || c == ':' {
            i += 1;
            let w = word(&mut i);
            let sigil = c.to_string();
            tokens.push(if c == '#' {
                Token::Name(sigil + &w)
            } else {
                Token::Value(sigil + &w)
            });
        } else if c.is_ascii_digit() {
            tokens.push(Token::Index(word(&mut i).parse().ok()?));
        } else if c.is_ascii_alphabetic() {
            tokens.push(Token::Word(word(&mut i)));
        } else if "=+-,().[]".contains(c) {
            tokens.push(Token::Punct(c));
            i += 1;
        } else {
            return None;
        }
    }
    Some(tokens)
}

enum Step {
    Name(String),
    Index(usize),
}

/// A path's steps; a list index in it puts the result of writing it beyond this preview.
struct Path {
    steps: Vec<Step>,
    indexed: bool,
}

struct Parser<'a> {
    tokens: Vec<Token>,
    at: usize,
    names: &'a HashMap<String, String>,
    values: &'a HashMap<String, AttributeValue>,
    item: &'a Item,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at)
    }

    fn next(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.at).cloned();
        self.at += 1;
        t
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(&Token::Punct(c)) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    /// `#a.#b[1]`, returning the display text and the steps.
    fn path(&mut self) -> Option<(String, Path)> {
        let Some(Token::Name(first)) = self.next() else {
            return None;
        };
        let first = self.names.get(&first)?.clone();
        let mut text = first.clone();
        let mut path = Path {
            steps: vec![Step::Name(first)],
            indexed: false,
        };
        loop {
            if self.eat('.') {
                let Some(Token::Name(n)) = self.next() else {
                    return None;
                };
                let name = self.names.get(&n)?.clone();
                text.push('.');
                text.push_str(&name);
                path.steps.push(Step::Name(name));
            } else if self.eat('[') {
                let Some(Token::Index(i)) = self.next() else {
                    return None;
                };
                self.eat(']').then_some(())?;
                text.push_str(&format!("[{i}]"));
                path.steps.push(Step::Index(i));
                path.indexed = true;
            } else {
                break;
            }
        }
        Some((text, path))
    }

    fn lookup(&self, path: &Path) -> Option<AttributeValue> {
        let mut value: Option<&AttributeValue> = None;
        for step in &path.steps {
            value = Some(match (value, step) {
                (None, Step::Name(name)) => self.item.get(name)?,
                (Some(AttributeValue::M(m)), Step::Name(name)) => m.get(name)?,
                (Some(AttributeValue::L(l)), Step::Index(i)) => l.get(*i)?,
                _ => return None,
            });
        }
        value.cloned()
    }

    /// An operand: a path, a value placeholder or a function. The outer `None` means the
    /// syntax is not understood; the inner one that the value cannot be worked out.
    fn operand(&mut self) -> Option<Option<AttributeValue>> {
        match self.peek()?.clone() {
            Token::Name(_) => {
                let (_, path) = self.path()?;
                Some(self.lookup(&path))
            }
            Token::Value(v) => {
                self.at += 1;
                Some(self.values.get(&v).cloned())
            }
            Token::Word(f) => {
                self.at += 1;
                self.eat('(').then_some(())?;
                let result = match f.to_lowercase().as_str() {
                    "if_not_exists" => {
                        let (_, path) = self.path()?;
                        self.eat(',').then_some(())?;
                        let fallback = self.operand()?;
                        self.lookup(&path).or(fallback)
                    }
                    "list_append" => {
                        let a = self.operand()?;
                        self.eat(',').then_some(())?;
                        let b = self.operand()?;
                        match (a, b) {
                            (Some(AttributeValue::L(mut a)), Some(AttributeValue::L(b))) => {
                                a.extend(b);
                                Some(AttributeValue::L(a))
                            }
                            _ => None,
                        }
                    }
                    _ => return None,
                };
                self.eat(')').then_some(())?;
                Some(result)
            }
            _ => None,
        }
    }

    /// `operand`, `operand + operand` or `operand - operand`.
    fn value(&mut self) -> Option<Option<AttributeValue>> {
        let a = self.operand()?;
        let sign = if self.eat('+') {
            1
        } else if self.eat('-') {
            -1
        } else {
            return Some(a);
        };
        let b = self.operand()?;
        Some(match (a, b) {
            (Some(AttributeValue::N(a)), Some(AttributeValue::N(b))) => {
                add_numbers(&a, &b, sign).map(AttributeValue::N)
            }
            _ => None,
        })
    }
}

/// The changes `update` makes to `item`, in the order the expression names them. Empty when
/// the expression is not understood at all.
pub fn preview(
    item: &Item,
    update: &str,
    names: &HashMap<String, String>,
    values: &HashMap<String, AttributeValue>,
) -> Vec<Change> {
    let Some(tokens) = tokenize(update) else {
        return Vec::new();
    };
    let mut p = Parser {
        tokens,
        at: 0,
        names,
        values,
        item,
    };
    let mut changes = Vec::new();
    let mut clause = String::new();
    while let Some(token) = p.peek().cloned() {
        if let Token::Word(w) = &token {
            let upper = w.to_uppercase();
            if matches!(upper.as_str(), "SET" | "REMOVE" | "ADD" | "DELETE") {
                clause = upper;
                p.at += 1;
                continue;
            }
        }
        if p.eat(',') {
            continue;
        }
        let Some((text, path)) = p.path() else {
            return changes;
        };
        let before = p.lookup(&path);
        let after = match clause.as_str() {
            "SET" => {
                if !p.eat('=') {
                    return changes;
                }
                let Some(value) = p.value() else {
                    return changes;
                };
                value.map(Some)
            }
            "REMOVE" => Some(None),
            "ADD" | "DELETE" => {
                let Some(value) = p.operand() else {
                    return changes;
                };
                match (clause.as_str(), value) {
                    ("ADD", Some(v)) => add(before.as_ref(), v).map(Some),
                    ("DELETE", Some(v)) => delete(before.as_ref(), v),
                    _ => None,
                }
            }
            _ => return changes,
        };
        // A list index means the result is not worked out, whatever the expression.
        let after = if path.indexed { None } else { after };
        changes.push(Change {
            path: text,
            before,
            after,
        });
    }
    changes
}

/// ADD: a number is added to, a set gains the elements.
fn add(before: Option<&AttributeValue>, v: AttributeValue) -> Option<AttributeValue> {
    use AttributeValue as A;
    match (before, v) {
        (None, v) => Some(v),
        (Some(A::N(a)), A::N(b)) => add_numbers(a, &b, 1).map(A::N),
        (Some(A::Ss(a)), A::Ss(b)) => Some(A::Ss(union(a.clone(), b))),
        (Some(A::Ns(a)), A::Ns(b)) => Some(A::Ns(union(a.clone(), b))),
        _ => None,
    }
}

/// DELETE: the elements leave the set; an emptied set is removed.
fn delete(before: Option<&AttributeValue>, v: AttributeValue) -> Option<Option<AttributeValue>> {
    use AttributeValue as A;
    let left = |a: &Vec<String>, b: Vec<String>| -> Vec<String> {
        a.iter().filter(|x| !b.contains(x)).cloned().collect()
    };
    let result = match (before?, v) {
        (A::Ss(a), A::Ss(b)) => A::Ss(left(a, b)),
        (A::Ns(a), A::Ns(b)) => A::Ns(left(a, b)),
        _ => return None,
    };
    Some(match &result {
        A::Ss(v) | A::Ns(v) if v.is_empty() => None,
        _ => Some(result),
    })
}

fn union(mut a: Vec<String>, b: Vec<String>) -> Vec<String> {
    for x in b {
        if !a.contains(&x) {
            a.push(x);
        }
    }
    a
}

/// `a + sign * b` in exact decimal arithmetic, as DynamoDB does with up to 38 digits.
fn add_numbers(a: &str, b: &str, sign: i128) -> Option<String> {
    let (x, xs) = decimal(a)?;
    let (y, ys) = decimal(b)?;
    let scale = xs.max(ys);
    let sum = x
        .checked_mul(10i128.checked_pow(scale - xs)?)?
        .checked_add(sign * y.checked_mul(10i128.checked_pow(scale - ys)?)?)?;
    Some(format_decimal(sum, scale))
}

/// `-12.50` as (-1250, 2).
fn decimal(text: &str) -> Option<(i128, u32)> {
    let text = text.trim();
    let (int, frac) = text.split_once('.').unwrap_or((text, ""));
    let digits = format!("{int}{frac}");
    if digits.trim_start_matches(['-', '+']).is_empty()
        || !digits
            .trim_start_matches(['-', '+'])
            .chars()
            .all(|c| c.is_ascii_digit())
    {
        return None;
    }
    Some((digits.parse().ok()?, frac.len() as u32))
}

fn format_decimal(value: i128, scale: u32) -> String {
    let negative = value < 0;
    let digits = value.unsigned_abs().to_string();
    let scale = scale as usize;
    let digits = format!("{digits:0>width$}", width = scale + 1);
    let (int, frac) = digits.split_at(digits.len() - scale);
    let frac = frac.trim_end_matches('0');
    let sign = if negative { "-" } else { "" };
    if frac.is_empty() {
        format!("{sign}{int}")
    } else {
        format!("{sign}{int}.{frac}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expr::Placeholders;

    fn s(v: &str) -> AttributeValue {
        AttributeValue::S(v.into())
    }

    fn n(v: &str) -> AttributeValue {
        AttributeValue::N(v.into())
    }

    /// The update as typed in the popup, rewritten into placeholders like the app does.
    fn run(
        item: &Item,
        typed: &str,
    ) -> Vec<(
        String,
        Option<AttributeValue>,
        Option<Option<AttributeValue>>,
    )> {
        let mut ph = Placeholders::default();
        let update = ph.update(typed).unwrap();
        preview(item, &update, &ph.names, &ph.values)
            .into_iter()
            .map(|c| (c.path, c.before, c.after))
            .collect()
    }

    fn sample() -> Item {
        Item::from([
            ("views".to_string(), n("16.25")),
            ("status".to_string(), s("open")),
            (
                "tags".to_string(),
                AttributeValue::Ss(vec!["a".into(), "b".into()]),
            ),
            ("hist".to_string(), AttributeValue::L(vec![s("x")])),
            (
                "profile".to_string(),
                AttributeValue::M(Item::from([("city".to_string(), s("Busan"))])),
            ),
        ])
    }

    #[test]
    fn set_with_values_arithmetic_and_functions() {
        let item = sample();
        assert_eq!(
            run(
                &item,
                "SET views = views + 0.75, status = \"closed\", seen = if_not_exists(seen, 0)"
            ),
            [
                ("views".into(), Some(n("16.25")), Some(Some(n("17")))),
                ("status".into(), Some(s("open")), Some(Some(s("closed")))),
                ("seen".into(), None, Some(Some(n("0")))),
            ]
        );
        assert_eq!(
            run(
                &item,
                "SET hist = list_append(hist, [\"y\"]), profile.city = \"서울\""
            ),
            [
                (
                    "hist".into(),
                    Some(AttributeValue::L(vec![s("x")])),
                    Some(Some(AttributeValue::L(vec![s("x"), s("y")])))
                ),
                (
                    "profile.city".into(),
                    Some(s("Busan")),
                    Some(Some(s("서울")))
                ),
            ]
        );
    }

    #[test]
    fn remove_add_and_delete() {
        let item = sample();
        assert_eq!(
            run(
                &item,
                "REMOVE status ADD views -1, tags <<\"c\">> DELETE tags <<\"a\", \"b\">>"
            ),
            [
                ("status".into(), Some(s("open")), Some(None)),
                ("views".into(), Some(n("16.25")), Some(Some(n("15.25")))),
                (
                    "tags".into(),
                    Some(AttributeValue::Ss(vec!["a".into(), "b".into()])),
                    Some(Some(AttributeValue::Ss(vec![
                        "a".into(),
                        "b".into(),
                        "c".into()
                    ])))
                ),
                (
                    "tags".into(),
                    Some(AttributeValue::Ss(vec!["a".into(), "b".into()])),
                    Some(None)
                ),
            ],
            "each action previews against the item as it is now"
        );
    }

    #[test]
    fn list_indexes_and_type_mismatches_are_unknown() {
        let item = sample();
        let changes = run(&item, "SET hist[0] = \"z\", status = status + 1");
        assert_eq!(changes[0].0, "hist[0]");
        assert_eq!(
            changes[0].1,
            Some(s("x")),
            "the current value is still shown"
        );
        assert_eq!(changes[0].2, None);
        assert_eq!(changes[1].2, None, "string + number");
    }

    #[test]
    fn decimals_stay_exact() {
        assert_eq!(add_numbers("0.1", "0.2", 1).as_deref(), Some("0.3"));
        assert_eq!(add_numbers("10", "10.5", -1).as_deref(), Some("-0.5"));
        assert_eq!(
            add_numbers("12345678901234567890", "1", 1).as_deref(),
            Some("12345678901234567891")
        );
        assert_eq!(
            add_numbers("1e3", "1", 1),
            None,
            "exponents are left to DynamoDB"
        );
    }
}
