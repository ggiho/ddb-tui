//! Expression text with literal names and values, rewritten into DynamoDB placeholder form.
//! `status = "active" AND size(tags) > 2` becomes `#n0 = :v0 AND size(#n1) > :v1`, so reserved
//! words need no escaping and values need no `:name` bookkeeping. DynamoDB still validates the
//! expression itself; this only tokenizes.
//!
//! Strings are quoted ('x' or "x"), numbers are bare, `true`/`false`/`null` are literals, lists
//! are `[1, "a"]` and sets `<<"a", "b">>` (as in PartiQL), and an attribute whose name is not a
//! plain word can be written in backticks: `` `my key` ``.

use std::collections::HashMap;

use anyhow::{Result, bail};
use aws_sdk_dynamodb::types::AttributeValue;

const CONDITION_KEYWORDS: [&str; 5] = ["AND", "OR", "NOT", "BETWEEN", "IN"];
const UPDATE_KEYWORDS: [&str; 4] = ["SET", "REMOVE", "ADD", "DELETE"];
const CONDITION_FUNCTIONS: [&str; 6] = [
    "attribute_exists",
    "attribute_not_exists",
    "attribute_type",
    "begins_with",
    "contains",
    "size",
];
const UPDATE_FUNCTIONS: [&str; 2] = ["if_not_exists", "list_append"];

/// Which expression grammar the text follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Filter, condition and key condition expressions.
    Condition,
    /// Update expressions: `SET`, `REMOVE`, `ADD`, `DELETE`.
    Update,
}

/// One step of an attribute path: `a.b[0]` is `[Name(a), Name(b), Index(0)]`.
#[derive(Clone, Debug, PartialEq)]
pub enum Seg {
    Name(String),
    Index(u32),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Token {
    Path(Vec<Seg>),
    Value(AttributeValue),
    /// Upper-cased keyword of the grammar.
    Keyword(String),
    /// Lower-cased function name followed by `(`.
    Func(String),
    /// `( ) , = < > <= >= <> + -`
    Sym(&'static str),
}

pub fn tokenize(text: &str, mode: Mode) -> Result<Vec<Token>> {
    let (keywords, functions): (&[&str], &[&str]) = match mode {
        Mode::Condition => (&CONDITION_KEYWORDS, &CONDITION_FUNCTIONS),
        Mode::Update => (&UPDATE_KEYWORDS, &UPDATE_FUNCTIONS),
    };
    let mut lx = Lexer::new(text);
    let mut out: Vec<Token> = Vec::new();
    while let Some(c) = lx.peek() {
        let after_operand = matches!(
            out.last(),
            Some(Token::Path(_) | Token::Value(_) | Token::Sym(")"))
        );
        let token = match c {
            c if c.is_whitespace() => {
                lx.pos += 1;
                continue;
            }
            '(' | ')' | ',' | '=' | '+' => {
                lx.pos += 1;
                Token::Sym(match c {
                    '(' => "(",
                    ')' => ")",
                    ',' => ",",
                    '=' => "=",
                    _ => "+",
                })
            }
            '<' if lx.peek_at(1) == Some('<') => Token::Value(lx.literal()?),
            '<' | '>' => {
                lx.pos += 1;
                Token::Sym(match (c, lx.peek()) {
                    ('<', Some('=')) => {
                        lx.pos += 1;
                        "<="
                    }
                    ('<', Some('>')) => {
                        lx.pos += 1;
                        "<>"
                    }
                    ('>', Some('=')) => {
                        lx.pos += 1;
                        ">="
                    }
                    ('<', _) => "<",
                    _ => ">",
                })
            }
            // A minus after an operand subtracts (update expressions); otherwise it signs a number.
            '-' if after_operand || !lx.peek_at(1).is_some_and(|d| d.is_ascii_digit()) => {
                lx.pos += 1;
                Token::Sym("-")
            }
            '"' | '\'' | '[' | '-' | '0'..='9' => Token::Value(lx.literal()?),
            '`' => Token::Path(lx.path()?),
            c if is_word_start(c) => {
                let word = lx.peek_word();
                let len = word.chars().count();
                let upper = word.to_ascii_uppercase();
                let lower = word.to_ascii_lowercase();
                if keywords.contains(&upper.as_str()) {
                    lx.pos += len;
                    Token::Keyword(upper)
                } else if matches!(lower.as_str(), "true" | "false" | "null") {
                    Token::Value(lx.literal()?)
                } else if functions.contains(&lower.as_str())
                    && lx.next_non_space_after(len) == Some('(')
                {
                    lx.pos += len;
                    Token::Func(lower)
                } else {
                    Token::Path(lx.path()?)
                }
            }
            other => bail!("unexpected '{other}' at position {}", lx.pos + 1),
        };
        out.push(token);
    }
    if out.is_empty() {
        bail!("the expression is empty");
    }
    Ok(out)
}

/// Comma-separated attribute paths, as used by projection expressions.
pub fn projection_paths(text: &str) -> Result<Vec<Vec<Seg>>> {
    let mut lx = Lexer::new(text);
    let mut paths = Vec::new();
    loop {
        lx.skip_spaces();
        paths.push(lx.path()?);
        lx.skip_spaces();
        match lx.peek() {
            None => return Ok(paths),
            Some(',') => lx.pos += 1,
            Some(c) => bail!("unexpected '{c}'; separate attributes with commas"),
        }
    }
}

/// An attribute name as it would be typed: bare when it is a plain word, else in backticks.
pub fn name_text(name: &str) -> String {
    let plain = name.chars().next().is_some_and(is_word_start)
        && name.chars().all(is_word_char)
        && !CONDITION_KEYWORDS.contains(&name.to_ascii_uppercase().as_str())
        && !UPDATE_KEYWORDS.contains(&name.to_ascii_uppercase().as_str())
        && !matches!(
            name.to_ascii_lowercase().as_str(),
            "true" | "false" | "null"
        );
    if plain {
        name.to_string()
    } else {
        format!("`{name}`")
    }
}

/// A value as it would be typed, or `None` for types without literal syntax (binary, maps).
pub fn literal_text(v: &AttributeValue) -> Option<String> {
    let quote = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    Some(match v {
        AttributeValue::S(s) => quote(s),
        AttributeValue::N(n) => n.clone(),
        AttributeValue::Bool(b) => b.to_string(),
        AttributeValue::Null(_) => "null".into(),
        AttributeValue::L(items) => format!(
            "[{}]",
            items
                .iter()
                .map(literal_text)
                .collect::<Option<Vec<_>>>()?
                .join(", ")
        ),
        AttributeValue::Ss(v) => format!(
            "<<{}>>",
            v.iter().map(|s| quote(s)).collect::<Vec<_>>().join(", ")
        ),
        AttributeValue::Ns(v) => format!("<<{}>>", v.join(", ")),
        _ => return None,
    })
}

/// Placeholder maps shared by every expression of one request.
#[derive(Debug, Default)]
pub struct Placeholders {
    pub names: HashMap<String, String>,
    pub values: HashMap<String, AttributeValue>,
    by_name: HashMap<String, String>,
}

impl Placeholders {
    fn name(&mut self, name: &str) -> String {
        if let Some(p) = self.by_name.get(name) {
            return p.clone();
        }
        let p = format!("#n{}", self.by_name.len());
        self.by_name.insert(name.to_string(), p.clone());
        self.names.insert(p.clone(), name.to_string());
        p
    }

    fn value(&mut self, v: AttributeValue) -> String {
        let p = format!(":v{}", self.values.len());
        self.values.insert(p.clone(), v);
        p
    }

    fn path(&mut self, segs: &[Seg]) -> String {
        let mut out = String::new();
        for seg in segs {
            match seg {
                Seg::Name(n) => {
                    if !out.is_empty() {
                        out.push('.');
                    }
                    out.push_str(&self.name(n));
                }
                Seg::Index(i) => out.push_str(&format!("[{i}]")),
            }
        }
        out
    }

    /// Condition syntax, as used by filter and condition expressions.
    pub fn condition(&mut self, text: &str) -> Result<String> {
        let tokens = tokenize(text, Mode::Condition)?;
        Ok(self.render(tokens))
    }

    /// Update expression syntax: `SET a = a + 1, b = list_append(b, [1]) REMOVE c`.
    pub fn update(&mut self, text: &str) -> Result<String> {
        let tokens = tokenize(text, Mode::Update)?;
        Ok(self.render(tokens))
    }

    /// Comma-separated attribute paths, as used by projection expressions.
    pub fn projection(&mut self, text: &str) -> Result<String> {
        let paths = projection_paths(text)?;
        Ok(paths
            .iter()
            .map(|p| self.path(p))
            .collect::<Vec<_>>()
            .join(", "))
    }

    fn render(&mut self, tokens: Vec<Token>) -> String {
        let mut out = String::new();
        let mut prev: Option<Token> = None;
        for t in tokens {
            // No space at the start, after `(`, before `)` or `,`, or between a function and `(`.
            let tight = matches!(
                (&prev, &t),
                (None, _)
                    | (Some(Token::Sym("(")), _)
                    | (_, Token::Sym(")" | ","))
                    | (Some(Token::Func(_)), Token::Sym("("))
            );
            if !tight {
                out.push(' ');
            }
            let text = match &t {
                Token::Path(p) => self.path(p),
                Token::Value(v) => self.value(v.clone()),
                Token::Keyword(k) | Token::Func(k) => k.clone(),
                Token::Sym(s) => s.to_string(),
            };
            out.push_str(&text);
            prev = Some(t);
        }
        out
    }
}

fn is_word_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

struct Lexer {
    chars: Vec<char>,
    pos: usize,
}

impl Lexer {
    fn new(text: &str) -> Self {
        Self {
            chars: text.chars().collect(),
            pos: 0,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_at(&self, n: usize) -> Option<char> {
        self.chars.get(self.pos + n).copied()
    }

    fn skip_spaces(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.pos += 1;
        }
    }

    fn take_while(&mut self, f: impl Fn(char) -> bool) -> String {
        let start = self.pos;
        while self.peek().is_some_and(&f) {
            self.pos += 1;
        }
        self.chars[start..self.pos].iter().collect()
    }

    fn peek_word(&self) -> String {
        self.chars[self.pos..]
            .iter()
            .take_while(|c| is_word_char(**c))
            .collect()
    }

    fn next_non_space_after(&self, n: usize) -> Option<char> {
        self.chars[self.pos + n..]
            .iter()
            .copied()
            .find(|c| !c.is_whitespace())
    }

    /// `a.b[0].c`, with backticks around names that are not plain words.
    fn path(&mut self) -> Result<Vec<Seg>> {
        let mut segs = Vec::new();
        loop {
            let name = match self.peek() {
                Some('`') => self.quoted('`')?,
                Some(c) if is_word_start(c) => self.take_while(is_word_char),
                _ => bail!("expected an attribute name at position {}", self.pos + 1),
            };
            if name.is_empty() {
                bail!("empty attribute name at position {}", self.pos);
            }
            segs.push(Seg::Name(name));
            while self.peek() == Some('[') {
                self.pos += 1;
                let digits = self.take_while(|c| c.is_ascii_digit());
                if self.peek() != Some(']') {
                    bail!("a list index looks like [0]");
                }
                self.pos += 1;
                segs.push(Seg::Index(
                    digits
                        .parse()
                        .map_err(|_| anyhow::anyhow!("a list index looks like [0]"))?,
                ));
            }
            if self.peek() != Some('.') {
                return Ok(segs);
            }
            self.pos += 1;
        }
    }

    /// One value: string, number, `true`/`false`/`null`, `[list]` or `<<set>>`.
    fn literal(&mut self) -> Result<AttributeValue> {
        self.skip_spaces();
        match self.peek() {
            Some(q @ ('"' | '\'')) => Ok(AttributeValue::S(self.quoted(q)?)),
            Some('-' | '0'..='9') => Ok(AttributeValue::N(self.number()?)),
            Some('[') => {
                self.pos += 1;
                let items = self.sequence(']')?;
                Ok(AttributeValue::L(items))
            }
            Some('<') if self.peek_at(1) == Some('<') => {
                self.pos += 2;
                let items = self.sequence('>')?;
                if self.peek() != Some('>') {
                    bail!("a set ends with >>");
                }
                self.pos += 1;
                set(items)
            }
            Some(c) if is_word_start(c) => {
                let word = self.take_while(is_word_char).to_ascii_lowercase();
                match word.as_str() {
                    "true" => Ok(AttributeValue::Bool(true)),
                    "false" => Ok(AttributeValue::Bool(false)),
                    "null" => Ok(AttributeValue::Null(true)),
                    _ => bail!("'{word}' is not a value (quote strings)"),
                }
            }
            Some(c) => bail!("unexpected '{c}' at position {}", self.pos + 1),
            None => bail!("expected a value"),
        }
    }

    /// Comma-separated literals up to and including `close`.
    fn sequence(&mut self, close: char) -> Result<Vec<AttributeValue>> {
        let mut items = Vec::new();
        self.skip_spaces();
        if self.peek() == Some(close) {
            self.pos += 1;
            return Ok(items);
        }
        loop {
            items.push(self.literal()?);
            self.skip_spaces();
            match self.peek() {
                Some(',') => self.pos += 1,
                Some(c) if c == close => {
                    self.pos += 1;
                    return Ok(items);
                }
                _ => bail!("expected ',' or '{close}' at position {}", self.pos + 1),
            }
        }
    }

    /// Text between `quote` characters; a backslash escapes the next character.
    fn quoted(&mut self, quote: char) -> Result<String> {
        let start = self.pos + 1;
        self.pos += 1;
        let mut s = String::new();
        loop {
            match self.peek() {
                None => bail!("unterminated {quote} starting at position {start}"),
                Some('\\') if quote != '`' => {
                    self.pos += 1;
                    if let Some(c) = self.peek() {
                        s.push(c);
                        self.pos += 1;
                    }
                }
                Some(c) if c == quote => {
                    self.pos += 1;
                    return Ok(s);
                }
                Some(c) => {
                    s.push(c);
                    self.pos += 1;
                }
            }
        }
    }

    fn number(&mut self) -> Result<String> {
        let start = self.pos;
        if self.peek() == Some('-') {
            self.pos += 1;
        }
        // Take the whole alphanumeric run so `1x2` is rejected instead of read as `1` then `x2`.
        self.take_while(|c| c.is_ascii_alphanumeric() || c == '.');
        // Exponent signs: `1e-5`.
        if matches!(self.chars.get(self.pos.wrapping_sub(1)), Some('e' | 'E'))
            && matches!(self.peek(), Some('+' | '-'))
        {
            self.pos += 1;
            self.take_while(|c| c.is_ascii_digit());
        }
        let n: String = self.chars[start..self.pos].iter().collect();
        if n.parse::<f64>().is_err() {
            bail!("'{n}' is not a number (quote strings)");
        }
        Ok(n)
    }
}

/// `<<…>>` holds only strings (SS) or only numbers (NS), and DynamoDB rejects empty sets.
fn set(items: Vec<AttributeValue>) -> Result<AttributeValue> {
    if items.is_empty() {
        bail!("a set cannot be empty");
    }
    if items.iter().all(|v| matches!(v, AttributeValue::S(_))) {
        Ok(AttributeValue::Ss(
            items
                .into_iter()
                .filter_map(|v| v.as_s().ok().cloned())
                .collect(),
        ))
    } else if items.iter().all(|v| matches!(v, AttributeValue::N(_))) {
        Ok(AttributeValue::Ns(
            items
                .into_iter()
                .filter_map(|v| v.as_n().ok().cloned())
                .collect(),
        ))
    } else {
        bail!("a set holds only strings or only numbers")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> AttributeValue {
        AttributeValue::S(v.into())
    }

    fn n(v: &str) -> AttributeValue {
        AttributeValue::N(v.into())
    }

    #[test]
    fn names_and_values_become_placeholders() {
        let mut p = Placeholders::default();
        let e = p
            .condition(r#"status = "active" AND size(tags) > 2"#)
            .unwrap();
        assert_eq!(e, "#n0 = :v0 AND size(#n1) > :v1");
        assert_eq!(p.names["#n0"], "status");
        assert_eq!(p.names["#n1"], "tags");
        assert_eq!(p.values[":v0"], s("active"));
        assert_eq!(p.values[":v1"], n("2"));
    }

    #[test]
    fn repeated_names_share_a_placeholder() {
        let mut p = Placeholders::default();
        assert_eq!(
            p.condition("a > 1 OR a <= -2.5").unwrap(),
            "#n0 > :v0 OR #n0 <= :v1"
        );
        assert_eq!(p.values[":v1"], n("-2.5"));
        assert_eq!(p.names.len(), 1);
    }

    #[test]
    fn functions_paths_and_quoted_names() {
        let mut p = Placeholders::default();
        let e = p
            .condition("attribute_exists(profile.city) AND contains(tags[0], 'x') AND begins_with(`my key`, \"p\")")
            .unwrap();
        assert_eq!(
            e,
            "attribute_exists(#n0.#n1) AND contains(#n2[0], :v0) AND begins_with(#n3, :v1)"
        );
        assert_eq!(p.names["#n3"], "my key");
    }

    #[test]
    fn a_function_name_without_parentheses_is_an_attribute() {
        let mut p = Placeholders::default();
        assert_eq!(p.condition("size = 3").unwrap(), "#n0 = :v0");
        assert_eq!(p.names["#n0"], "size");
    }

    #[test]
    fn keywords_and_literals() {
        let mut p = Placeholders::default();
        let e = p
            .condition(r#"not (n between 1 and 5) and s in ("a", "b") and flag = TRUE and gone = null and x <> 'it\'s'"#)
            .unwrap();
        assert_eq!(
            e,
            "NOT (#n0 BETWEEN :v0 AND :v1) AND #n1 IN (:v2, :v3) AND #n2 = :v4 AND #n3 = :v5 AND #n4 <> :v6"
        );
        assert_eq!(p.values[":v4"], AttributeValue::Bool(true));
        assert_eq!(p.values[":v5"], AttributeValue::Null(true));
        assert_eq!(p.values[":v6"], s("it's"));
    }

    #[test]
    fn projection_shares_name_placeholders() {
        let mut p = Placeholders::default();
        p.condition("pk = 'a'").unwrap();
        assert_eq!(
            p.projection("pk, profile.city ,tags[1]").unwrap(),
            "#n0, #n1.#n2, #n3[1]"
        );
    }

    #[test]
    fn update_expressions() {
        let mut p = Placeholders::default();
        let e = p
            .update(r#"set views = views + 1, total = total - 2.5, tags = list_append(tags, ["x", 3]), seen = if_not_exists(seen, 0) remove old ADD labels <<"a", "b">> DELETE nums <<1, 2>>"#)
            .unwrap();
        assert_eq!(
            e,
            "SET #n0 = #n0 + :v0, #n1 = #n1 - :v1, #n2 = list_append(#n2, :v2), #n3 = if_not_exists(#n3, :v3) REMOVE #n4 ADD #n5 :v4 DELETE #n6 :v5"
        );
        assert_eq!(
            p.values[":v1"],
            n("2.5"),
            "a minus after an operand subtracts"
        );
        assert_eq!(p.values[":v2"], AttributeValue::L(vec![s("x"), n("3")]));
        assert_eq!(
            p.values[":v4"],
            AttributeValue::Ss(vec!["a".into(), "b".into()])
        );
        assert_eq!(
            p.values[":v5"],
            AttributeValue::Ns(vec!["1".into(), "2".into()])
        );
    }

    #[test]
    fn negative_numbers_after_operators_and_exponents() {
        let mut p = Placeholders::default();
        assert_eq!(
            p.update("SET a = -3, b = 1e-5").unwrap(),
            "SET #n0 = :v0, #n1 = :v1"
        );
        assert_eq!(p.values[":v0"], n("-3"));
        assert_eq!(p.values[":v1"], n("1e-5"));
    }

    #[test]
    fn literal_text_round_trips() {
        let values = [
            s("say \"hi\" \\ there"),
            n("-1.5"),
            AttributeValue::Bool(false),
            AttributeValue::Null(true),
            AttributeValue::L(vec![s("a"), AttributeValue::L(vec![n("1")])]),
            AttributeValue::Ss(vec!["x".into(), "y".into()]),
            AttributeValue::Ns(vec!["1".into(), "2".into()]),
        ];
        for v in values {
            let text = format!("a = {}", literal_text(&v).unwrap());
            let tokens = tokenize(&text, Mode::Condition).unwrap();
            assert_eq!(tokens[2], Token::Value(v), "{text}");
        }
        assert_eq!(name_text("status"), "status");
        assert_eq!(name_text("my key"), "`my key`");
        assert_eq!(name_text("in"), "`in`");
    }

    #[test]
    fn reports_mistakes() {
        let mut p = Placeholders::default();
        assert!(
            p.condition("a = \"open")
                .unwrap_err()
                .to_string()
                .contains("unterminated")
        );
        assert!(
            p.condition("a = $")
                .unwrap_err()
                .to_string()
                .contains("unexpected '$'")
        );
        assert!(p.condition("a[x] = 1").is_err());
        assert!(p.condition("  ").is_err());
        assert!(p.projection("a b").is_err());
        assert!(p.condition("a = 1x2").is_err());
        assert!(p.update("ADD s <<>>").is_err(), "empty set");
        assert!(p.update("ADD s <<1, \"a\">>").is_err(), "mixed set");
        assert!(p.update("SET l = [1, 2").is_err());
    }
}
