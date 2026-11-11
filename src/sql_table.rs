//! Reader for cli_helpers ascii_escaped output. Values are display text, not
//! typed database values. Callers must select ascii_escaped to preserve row boundaries.
use unicode_width::UnicodeWidthChar;

#[derive(Debug)]
pub struct SqlTable {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

pub fn parse_escaped(input: &str) -> Option<SqlTable> {
    let clean = strip_sgr(input)?;
    let lines: Vec<_> = clean.lines().collect();
    let start = lines.iter().position(|l| l.starts_with('+'))?;
    let border = *lines.get(start)?;
    if !border.chars().all(|c| c == '+' || c == '-') || !border.ends_with('+') {
        return None;
    }
    let edges: Vec<_> = border
        .char_indices()
        .filter_map(|(i, c)| (c == '+').then_some(i))
        .collect();
    if edges.len() < 2 || edges.windows(2).any(|w| w[1] - w[0] < 3) {
        return None;
    }
    let headers = cells(lines.get(start + 1)?, &edges)?;
    let sep = *lines.get(start + 2)?;
    if sep != border && sep != format!("|{}|", &border[1..border.len() - 1]) {
        return None;
    }
    let mut rows = Vec::new();
    let mut end = None;
    for (i, line) in lines.iter().enumerate().skip(start + 3) {
        if *line == border {
            end = Some(i);
            break;
        }
        let row = cells(line, &edges)?;
        // ggiho/mycli may lay single-column results out as an ls-style grid.
        // Its two-space separators are not SQL column boundaries.
        if row.len() == 1 && row[0].contains("  ") {
            return None;
        }
        rows.push(row);
    }
    let end = end?;
    if lines[..start]
        .iter()
        .chain(lines[end + 1..].iter())
        .any(|l| l.starts_with('|') || l.starts_with('+'))
    {
        return None;
    }
    Some(SqlTable { headers, rows })
}

fn cells(line: &str, edges: &[usize]) -> Option<Vec<String>> {
    let mut column = 0;
    let mut boundary = 0;
    let mut values = Vec::new();
    let mut value = String::new();
    for c in line.chars() {
        if edges.get(boundary) == Some(&column) {
            if c != '|' {
                return None;
            }
            if boundary > 0 {
                values.push(value.trim().to_owned());
                value.clear();
            }
            boundary += 1;
        } else {
            if boundary == 0 || boundary == edges.len() {
                return None;
            }
            value.push(c);
        }
        column += c.width()?;
    }
    (boundary == edges.len() && column == edges.last()? + 1).then_some(values)
}

fn strip_sgr(input: &str) -> Option<String> {
    let mut chars = input.chars();
    let mut out = String::new();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.next()? != '[' {
                return None;
            }
            loop {
                let c = chars.next()?;
                if c == 'm' {
                    break;
                }
                if !c.is_ascii_digit() && c != ';' && c != ':' {
                    return None;
                }
            }
        } else if c.is_control() && c != '\n' && c != '\r' {
            return None;
        } else {
            out.push(c);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    const ASCII: &str = "+----+-------+\n| id | value |\n+----+-------+\n| 1  | a|b   |\n| 2  | 서울  |\n+----+-------+";
    #[test]
    fn both_formats_preserve_embedded_delimiters_and_wide_text() {
        for text in [
            ASCII.to_string(),
            ASCII.replacen("\n+----+-------+\n", "\n|----+-------|\n", 1),
        ] {
            let t = parse_escaped(&text).unwrap();
            assert_eq!(t.headers, ["id", "value"]);
            assert_eq!(t.rows, vec![vec!["1", "a|b"], vec!["2", "서울"]]);
        }
    }
    #[test]
    fn accepts_color_and_footer() {
        assert_eq!(
            parse_escaped(&format!("\x1b[32m{ASCII}\x1b[0m\n2 rows in set"))
                .unwrap()
                .rows
                .len(),
            2
        );
    }
    #[test]
    fn accepts_empty_fields_and_refuses_multiple_results() {
        assert!(parse_escaped(&ASCII.replace("| 2  | 서울  |", "|    | b     |")).is_some());
        assert!(parse_escaped(&format!("{ASCII}\n{ASCII}")).is_none());
        assert!(parse_escaped("arbitrary output").is_none());
        assert!(parse_escaped(&ASCII.replace("서울", "\x1b]52;c;bad\x07")).is_none());
    }
    #[test]
    fn empty_result_has_headers() {
        let t = parse_escaped("+----+\n| id |\n+----+\n+----+").unwrap();
        assert!(t.rows.is_empty());
    }
}
