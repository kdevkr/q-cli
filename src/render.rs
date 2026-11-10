//! Render a deserialized `K` value as readable text or as JSON.
//! Row caps take a `max` argument; `max == 0` means unlimited.

use std::fmt::Write as _;

use unicode_width::UnicodeWidthStr;

use crate::k;
use crate::k::{K, NULL_INT, NULL_LONG, NULL_SHORT};
use crate::query;

// ============================================================================
// Shared scalar formatting
// ============================================================================

fn str_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

fn fmt_float(v: f64) -> String {
    if v.is_nan() {
        "0n".into()
    } else if v.is_infinite() {
        if v > 0.0 {
            "0w".into()
        } else {
            "-0w".into()
        }
    } else {
        format!("{v}")
    }
}

/// Plain string for a single scalar (also used for table cells in text mode).
#[must_use]
pub fn scalar_to_string(val: &K) -> String {
    match val {
        K::Bool(b) => (if *b { "1" } else { "0" }).into(),
        K::Byte(x) => format!("0x{x:02x}"),
        K::Short(v) => {
            if *v == NULL_SHORT {
                "0N".into()
            } else if *v == i16::MAX {
                "0Wh".into()
            } else if *v == -i16::MAX {
                "-0Wh".into()
            } else {
                v.to_string()
            }
        }
        K::Int(v) => {
            if *v == NULL_INT {
                "0N".into()
            } else if *v == i32::MAX {
                "0Wi".into()
            } else if *v == -i32::MAX {
                "-0Wi".into()
            } else {
                v.to_string()
            }
        }
        K::Long(v) => {
            if *v == NULL_LONG {
                "0N".into()
            } else if *v == i64::MAX {
                "0Wj".into()
            } else if *v == -i64::MAX {
                "-0Wj".into()
            } else {
                v.to_string()
            }
        }
        K::Real(v) => fmt_float(*v as f64),
        K::Float(v) => fmt_float(*v),
        K::Char(c) => c.to_string(),
        K::Symbol(s) => {
            if s.is_empty() {
                "`".into()
            } else {
                s.clone()
            }
        }
        K::Timestamp(v) => {
            if *v == NULL_LONG {
                "0Np".into()
            } else {
                k::fmt_timestamp(*v)
            }
        }
        K::Month(v) => {
            if *v == NULL_INT {
                "0Nm".into()
            } else {
                k::fmt_month(*v)
            }
        }
        K::Date(v) => {
            if *v == NULL_INT {
                "0Nd".into()
            } else {
                k::fmt_date(*v)
            }
        }
        K::Datetime(v) => {
            if v.is_nan() {
                "0Nz".into()
            } else {
                k::fmt_datetime(*v)
            }
        }
        K::Timespan(v) => {
            if *v == NULL_LONG {
                "0Nn".into()
            } else {
                k::fmt_timespan(*v)
            }
        }
        K::Minute(v) => {
            if *v == NULL_INT {
                "0Nu".into()
            } else {
                k::fmt_minute(*v)
            }
        }
        K::Second(v) => {
            if *v == NULL_INT {
                "0Nv".into()
            } else {
                k::fmt_second(*v)
            }
        }
        K::Time(v) => {
            if *v == NULL_INT {
                "0Nt".into()
            } else {
                k::fmt_time(*v)
            }
        }
        K::Guid(g) => k::fmt_guid(g),
        K::Null => "::".into(),
        K::Function(s) => s.clone(),
        K::Projection(v) => {
            if v.is_empty() {
                "{}".to_string()
            } else {
                let f = scalar_to_string(&v[0]);
                let arg_parts: Vec<String> = v[1..]
                    .iter()
                    .map(|arg| match arg {
                        K::Null => String::new(),
                        other => scalar_to_string(other),
                    })
                    .collect();
                let args = arg_parts.join(";");
                format!("{f}[{args}]")
            }
        }
        // nested table/dict in a scalar slot -> compact summary (no recursion)
        K::Table(_) => format!("(table: {} rows)", table_rows(val).unwrap_or(0)),
        K::Dict(_, _) => "(dict)".into(),
        other => compact_vec(other, 0),
    }
}

/// Cell string for CSV (nulls format as empty strings per CSV standard).
fn scalar_to_csv(val: &K) -> String {
    match val {
        K::Short(v) if *v == NULL_SHORT => String::new(),
        K::Int(v) | K::Month(v) | K::Date(v) | K::Minute(v) | K::Second(v) | K::Time(v)
            if *v == NULL_INT =>
        {
            String::new()
        }
        K::Long(v) | K::Timestamp(v) | K::Timespan(v) if *v == NULL_LONG => String::new(),
        K::Real(v) if v.is_nan() => String::new(),
        K::Float(v) | K::Datetime(v) if v.is_nan() => String::new(),
        K::Symbol(s) | K::CharV(s) => s.clone(),
        K::Null => String::new(),
        _ => scalar_to_string(val),
    }
}

fn scalar_sym_aware(val: &K) -> String {
    match val {
        K::Symbol(s) => format!("`{s}"),
        _ => scalar_to_string(val),
    }
}

/// Compact one-line form for a vector (used inside lists / nested cells / vector rendering).
fn compact_vec(val: &K, max: usize) -> String {
    match val {
        K::CharV(s) => {
            let escaped = query::q_escape(s);
            format!("\"{escaped}\"")
        }
        K::ByteV(v) => {
            let n = v.len();
            let shown = if max == 0 { n } else { n.min(max) };
            let mut s = String::from("0x");
            for b in &v[..shown] {
                write!(s, "{b:02x}").ok();
            }
            if shown < n {
                s.push_str("...");
            }
            s
        }
        K::BoolV(v) => {
            let n = v.len();
            if n == 0 {
                return "0#0b".to_string();
            }
            let shown = if max == 0 { n } else { n.min(max) };
            let mut s = String::with_capacity(shown + 2);
            for b in &v[..shown] {
                s.push(if *b { '1' } else { '0' });
            }
            s.push('b');
            if shown < n {
                s.push_str("...");
            }
            s
        }
        K::SymbolV(v) => {
            let n = v.len();
            let shown = if max == 0 { n } else { n.min(max) };
            let mut s: String = v[..shown].iter().map(|sym| format!("`{sym}")).collect();
            if shown < n {
                s.push_str("...");
            }
            s
        }
        K::List(v) => {
            let n = v.len();
            let shown = if max == 0 { n } else { n.min(max) };
            let parts: Vec<String> = v[..shown].iter().map(scalar_sym_aware).collect();
            let mut inner = parts.join(";");
            if shown < n {
                inner.push_str(";...");
            }
            format!("({inner})")
        }
        _ => {
            let n = k::len_of(val);
            let shown = if max == 0 { n } else { n.min(max) };
            let parts: Vec<String> = (0..shown)
                .map(|i| scalar_to_string(&k::at(val, i)))
                .collect();
            let mut items = parts.join(" ");
            if shown < n {
                items.push_str(" ...");
            }
            items
        }
    }
}

// ============================================================================
// Table extraction
// ============================================================================

fn is_table(val: &K) -> bool {
    matches!(val, K::Table(_))
}

fn dict_parts(d: &K) -> Option<(&[String], &[K])> {
    if let K::Dict(keys, vals) = d {
        if let (K::SymbolV(names), K::List(cols)) = (&**keys, &**vals) {
            if names.len() == cols.len() {
                return Some((names.as_slice(), cols.as_slice()));
            }
        }
    }
    None
}

/// (column names, columns) for a table or keyed table, borrowing elements without cloning columns.
fn table_parts(val: &K) -> Option<(Vec<&str>, Vec<&K>)> {
    match val {
        K::Table(d) => {
            let (names, cols) = dict_parts(d)?;
            Some((
                names.iter().map(String::as_str).collect(),
                cols.iter().collect(),
            ))
        }
        K::Dict(kk, vv) if is_table(kk) && is_table(vv) => {
            let (mut names, mut cols) = table_parts(kk)?;
            let (vn, vc) = table_parts(vv)?;
            names.extend(vn);
            cols.extend(vc);
            Some((names, cols))
        }
        _ => None,
    }
}

/// Row count if `val` is a table or keyed table (used for truncation signalling).
#[must_use]
pub fn table_rows(val: &K) -> Option<usize> {
    match val {
        K::Table(d) => dict_parts(d).map(|(_, cols)| cols.first().map_or(0, k::len_of)),
        K::Dict(kk, vv) if is_table(kk) && is_table(vv) => table_rows(kk),
        _ => None,
    }
}

fn cap(nrows: usize, max: usize) -> usize {
    if max == 0 {
        nrows
    } else {
        nrows.min(max)
    }
}

// ============================================================================
// Text rendering
// ============================================================================

#[must_use]
pub fn to_text(val: &K, max: usize) -> String {
    if let Some((names, cols)) = table_parts(val) {
        return render_table(&names, &cols, max);
    }
    match val {
        K::Dict(keys, vals) => render_dict(keys, vals, max),
        K::CharV(s) => s.clone(),
        K::List(v) => {
            let n = v.len();
            let shown = if max == 0 { n } else { n.min(max) };
            let mut parts = Vec::new();
            for item in &v[..shown] {
                if table_parts(item).is_some() || matches!(item, K::Dict(_, _)) {
                    for l in to_text(item, max).lines() {
                        parts.push(format!("  {l}"));
                    }
                } else {
                    parts.push(scalar_sym_aware(item));
                }
            }
            if shown < n {
                let remaining = n - shown;
                parts.push(format!("  ... ({remaining} more items)"));
            }
            parts.join("\n")
        }
        K::Null => "::".into(),
        K::Symbol(s) => format!("`{s}"),
        atom_or_vec => {
            if k::len_of(atom_or_vec) > 1 {
                compact_vec(atom_or_vec, max)
            } else {
                scalar_to_string(atom_or_vec)
            }
        }
    }
}

fn render_table(names: &[&str], cols: &[&K], max: usize) -> String {
    let nrows = cols.first().copied().map_or(0, k::len_of);
    let shown = cap(nrows, max);

    let mut widths: Vec<usize> = names.iter().map(|n| str_width(n)).collect();
    let mut cells: Vec<Vec<String>> = Vec::with_capacity(cols.len());
    for (ci, &col) in cols.iter().enumerate() {
        let mut colcells = Vec::with_capacity(shown);
        for i in 0..shown {
            let s = scalar_to_string(&k::at(col, i));
            widths[ci] = widths[ci].max(str_width(&s));
            colcells.push(s);
        }
        cells.push(colcells);
    }

    let mut out = String::new();
    for (ci, name) in names.iter().enumerate() {
        if ci > 0 {
            out.push(' ');
        }
        out.push_str(&pad(name, widths[ci]));
    }
    out.push('\n');
    let width_sum: usize = widths.iter().sum();
    let total = width_sum + names.len().saturating_sub(1);
    out.push_str(&"-".repeat(total));
    out.push('\n');
    for i in 0..shown {
        for (ci, (col_cells, &w)) in cells.iter().zip(&widths).enumerate() {
            if ci > 0 {
                out.push(' ');
            }
            out.push_str(&pad(&col_cells[i], w));
        }
        out.push('\n');
    }
    if nrows > shown {
        let more = nrows - shown;
        let _ = writeln!(out, "..({more} more of {nrows} rows)");
    }
    out.trim_end().to_string()
}

fn pad(s: &str, w: usize) -> String {
    let sw = str_width(s);
    if sw >= w {
        s.to_string()
    } else {
        let spaces = " ".repeat(w - sw);
        format!("{s}{spaces}")
    }
}

/// Dict as `key| value` lines; a table-valued or dict-valued entry is rendered
/// as an indented sub-block (so e.g. `describe`'s columns/sample and nested dicts show in full).
fn render_dict(keys: &K, vals: &K, max: usize) -> String {
    let n = k::len_of(keys);
    let shown = cap(n, max);
    let kw = (0..shown)
        .map(|i| str_width(&scalar_to_string(&k::at(keys, i))))
        .max()
        .unwrap_or(0);
    let mut lines: Vec<String> = Vec::new();
    for i in 0..shown {
        let kstr = scalar_to_string(&k::at(keys, i));
        let v = k::at(vals, i);
        let padded_key = pad(&kstr, kw);
        if table_parts(&v).is_some() || matches!(v, K::Dict(_, _)) {
            lines.push(format!("{padded_key}|"));
            for l in to_text(&v, max).lines() {
                lines.push(format!("  {l}"));
            }
        } else {
            let val_str = scalar_sym_aware(&v);
            lines.push(format!("{padded_key}| {val_str}"));
        }
    }
    if n > shown {
        let more = n - shown;
        lines.push(format!("..({more} more of {n} entries)"));
    }
    lines.join("\n")
}

// ============================================================================
// CSV rendering (tables only; non-tables fall back to text). Uncapped.
// ============================================================================

#[must_use]
pub fn to_csv(val: &K) -> String {
    let Some((names, cols)) = table_parts(val) else {
        return to_text(val, 0);
    };
    let nrows = cols.first().copied().map_or(0, k::len_of);
    let mut out = String::new();
    let header_fields: Vec<String> = names
        .iter()
        .map(|n| csv_field(&defuse_formula(n.to_string())))
        .collect();
    out.push_str(&header_fields.join(","));
    out.push('\n');
    for i in 0..nrows {
        let row: Vec<String> = cols
            .iter()
            .map(|&c| {
                let cell = k::at(c, i);
                let text = scalar_to_csv(&cell);
                // Only text cells: a numeric `-5` must stay a number.
                if matches!(cell, K::Symbol(_) | K::CharV(_) | K::Char(_)) {
                    csv_field(&defuse_formula(text))
                } else {
                    csv_field(&text)
                }
            })
            .collect();
        out.push_str(&row.join(","));
        out.push('\n');
    }
    out.pop();
    out
}

/// Spreadsheet apps run a cell starting with `=`, `+`, `-`, `@`, tab or CR as a
/// formula (`=cmd|'/c calc'!A1`). Prefixing `'` makes them show it as text.
fn defuse_formula(mut s: String) -> String {
    if s.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        s.insert(0, '\'');
    }
    s
}

fn csv_field(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        let escaped = s.replace('"', "\"\"");
        format!("\"{escaped}\"")
    } else {
        s.to_string()
    }
}

// ============================================================================
// JSON rendering
// ============================================================================

#[must_use]
pub fn to_json(val: &K, max: usize) -> String {
    if let Some((names, cols)) = table_parts(val) {
        return json_rows(&names, &cols, max);
    }
    json_frag(val, max)
}

fn json_rows(names: &[&str], cols: &[&K], max: usize) -> String {
    let nrows = cap(cols.first().copied().map_or(0, k::len_of), max);
    let mut out = String::from("[");
    for i in 0..nrows {
        if i > 0 {
            out.push(',');
        }
        out.push('{');
        for (ci, name) in names.iter().enumerate() {
            if ci > 0 {
                out.push(',');
            }
            out.push_str(&json_string(name));
            out.push(':');
            out.push_str(&json_frag(&k::at(cols[ci], i), 0));
        }
        out.push('}');
    }
    out.push(']');
    out
}

fn json_frag(val: &K, max: usize) -> String {
    match val {
        K::Bool(b) => (if *b { "true" } else { "false" }).into(),
        K::Byte(x) => x.to_string(),
        K::Short(v) => num_or_null(*v == NULL_SHORT, *v as i64),
        K::Int(v) => num_or_null(*v == NULL_INT, *v as i64),
        K::Long(v) => num_or_null(*v == NULL_LONG, *v),
        K::Real(v) => float_json(*v as f64),
        K::Float(v) => float_json(*v),
        K::Char(c) => json_string(&c.to_string()),
        K::Symbol(s) => json_string(s),
        K::Timestamp(v) => {
            if *v == NULL_LONG {
                "null".into()
            } else {
                json_string(&k::fmt_timestamp(*v))
            }
        }
        K::Month(v) => {
            if *v == NULL_INT {
                "null".into()
            } else {
                json_string(&k::fmt_month(*v))
            }
        }
        K::Date(v) => {
            if *v == NULL_INT {
                "null".into()
            } else {
                json_string(&k::fmt_date(*v))
            }
        }
        K::Datetime(v) => {
            if v.is_nan() {
                "null".into()
            } else {
                json_string(&k::fmt_datetime(*v))
            }
        }
        K::Timespan(v) => {
            if *v == NULL_LONG {
                "null".into()
            } else {
                json_string(&k::fmt_timespan(*v))
            }
        }
        K::Minute(v) => {
            if *v == NULL_INT {
                "null".into()
            } else {
                json_string(&k::fmt_minute(*v))
            }
        }
        K::Second(v) => {
            if *v == NULL_INT {
                "null".into()
            } else {
                json_string(&k::fmt_second(*v))
            }
        }
        K::Time(v) => {
            if *v == NULL_INT {
                "null".into()
            } else {
                json_string(&k::fmt_time(*v))
            }
        }
        K::Guid(g) => json_string(&k::fmt_guid(g)),
        K::CharV(s) => json_string(s),
        K::SymbolV(v) => {
            let n = v.len();
            let shown = if max == 0 { n } else { n.min(max) };
            let items: Vec<String> = v[..shown].iter().map(|s| json_string(s)).collect();
            let joined = items.join(",");
            format!("[{joined}]")
        }
        K::List(v) => {
            let n = v.len();
            let shown = if max == 0 { n } else { n.min(max) };
            let items: Vec<String> = v[..shown].iter().map(|item| json_frag(item, max)).collect();
            let joined = items.join(",");
            format!("[{joined}]")
        }
        K::Dict(keys, vals) => json_dict(keys, vals, max),
        K::Table(_) => {
            if let Some((names, cols)) = table_parts(val) {
                json_rows(&names, &cols, max)
            } else {
                "null".into()
            }
        }
        K::Null => "null".into(),
        K::Function(s) => json_string(s),
        K::Projection(_) => json_string(&scalar_to_string(val)),
        other => {
            let n = k::len_of(other);
            let shown = if max == 0 { n } else { n.min(max) };
            let items: Vec<String> = (0..shown)
                .map(|i| json_frag(&k::at(other, i), max))
                .collect();
            let joined = items.join(",");
            format!("[{joined}]")
        }
    }
}

fn json_dict(keys: &K, vals: &K, max: usize) -> String {
    if let K::SymbolV(names) = keys {
        let n = names.len();
        let shown = if max == 0 { n } else { n.min(max) };
        let mut out = String::from("{");
        for (i, name) in names[..shown].iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&json_string(name));
            out.push(':');
            out.push_str(&json_frag(&k::at(vals, i), max));
        }
        out.push('}');
        out
    } else if is_table(keys) && is_table(vals) {
        if let (Some((mut names, mut cols)), Some((vn, vc))) =
            (table_parts(keys), table_parts(vals))
        {
            names.extend(vn);
            cols.extend(vc);
            return json_rows(&names, &cols, max);
        }
        "null".into()
    } else {
        let keys_json = json_frag(keys, max);
        let vals_json = json_frag(vals, max);
        format!("{{\"keys\":{keys_json},\"values\":{vals_json}}}")
    }
}

fn num_or_null(is_null: bool, v: i64) -> String {
    if is_null {
        "null".into()
    } else {
        v.to_string()
    }
}

fn float_json(v: f64) -> String {
    if v.is_finite() {
        format!("{v}")
    } else {
        "null".into()
    }
}

/// Make server-supplied text safe to print on a terminal: control characters
/// (ESC, CR, BEL, C1, ...) become visible `\x1b`-style escapes so data cannot
/// drive the terminal. Newlines, tabs and CRLF line ends are kept.
#[must_use]
pub fn escape_control(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        let keep = !c.is_control()
            || c == '\n'
            || c == '\t'
            || (c == '\r' && chars.peek() == Some(&'\n'));
        if keep {
            out.push(c);
        } else {
            let _ = write!(out, "\\x{:02x}", c as u32);
        }
    }
    out
}

/// JSON-escape a string (public so error output can reuse it).
#[must_use]
pub fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let code = c as u32;
                out.push_str(&format!("\\u{code:04x}"));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // table: a:1 2 (with a null), b:`x`y
    fn sample_table() -> K {
        K::Table(Box::new(K::Dict(
            Box::new(K::SymbolV(vec!["a".into(), "b".into()])),
            Box::new(K::List(vec![
                K::IntV(vec![1, NULL_INT]),
                K::SymbolV(vec!["x".into(), "y".into()]),
            ])),
        )))
    }

    #[test]
    fn table_to_json_rows() {
        assert_eq!(
            to_json(&sample_table(), 0),
            r#"[{"a":1,"b":"x"},{"a":null,"b":"y"}]"#
        );
    }

    #[test]
    fn table_to_csv_with_null_cell() {
        assert_eq!(to_csv(&sample_table()), "a,b\n1,x\n,y");
    }

    #[test]
    fn csv_preserves_trailing_whitespace_and_empty_rows() {
        for (values, expected) in [
            (vec!["abc \t"], "s\nabc \t"),
            (vec!["abc", "", ""], "s\nabc\n\n"),
            (vec![""], "s\n"),
            (vec![], "s"),
        ] {
            let table = K::Table(Box::new(K::Dict(
                Box::new(K::SymbolV(vec!["s".to_string()])),
                Box::new(K::List(vec![K::SymbolV(
                    values.into_iter().map(str::to_string).collect(),
                )])),
            )));
            assert_eq!(to_csv(&table), expected);
        }
    }

    #[test]
    fn json_respects_max_rows() {
        assert_eq!(to_json(&sample_table(), 1), r#"[{"a":1,"b":"x"}]"#);
    }

    #[test]
    fn escape_control_neutralises_terminal_sequences() {
        assert_eq!(escape_control("plain\ttext\nline2\r\n"), "plain\ttext\nline2\r\n");
        assert_eq!(escape_control("\u{1b}[31mRED\u{7}"), "\\x1b[31mRED\\x07");
        assert_eq!(escape_control("over\rwrite"), "over\\x0dwrite");
        assert_eq!(escape_control("c1\u{9b}2J"), "c1\\x9b2J");
    }

    #[test]
    fn csv_field_quotes_when_needed() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("she said \"hi\""), "\"she said \"\"hi\"\"\"");
    }

    #[test]
    fn scalar_nulls_and_floats() {
        assert_eq!(scalar_to_string(&K::Int(NULL_INT)), "0N");
        assert_eq!(scalar_to_string(&K::Int(i32::MAX)), "0Wi");
        assert_eq!(scalar_to_string(&K::Int(-i32::MAX)), "-0Wi");
        assert_eq!(scalar_to_string(&K::Long(i64::MAX)), "0Wj");
        assert_eq!(scalar_to_string(&K::Long(-i64::MAX)), "-0Wj");
        assert_eq!(scalar_to_string(&K::Short(i16::MAX)), "0Wh");
        assert_eq!(scalar_to_string(&K::Float(f64::NAN)), "0n");
        assert_eq!(scalar_to_string(&K::Symbol("".into())), "`");
        assert_eq!(scalar_to_string(&K::Long(42)), "42");
        assert_eq!(fmt_float(f64::INFINITY), "0w");
        assert_eq!(fmt_float(f64::NAN), "0n");
    }

    #[test]
    fn unicode_table_column_alignment() {
        let table = K::Table(Box::new(K::Dict(
            Box::new(K::SymbolV(vec!["k".into(), "v".into()])),
            Box::new(K::List(vec![
                K::SymbolV(vec!["가나다".into(), "a".into()]),
                K::IntV(vec![1, 2]),
            ])),
        )));
        let text = to_text(&table, 0);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0], "k      v");
        assert_eq!(lines[1], "--------");
        assert_eq!(lines[2], "가나다 1");
        assert_eq!(lines[3], "a      2");
    }

    #[test]
    fn nested_dict_renders_indented() {
        let inner = K::Dict(
            Box::new(K::SymbolV(vec!["x".into(), "y".into()])),
            Box::new(K::IntV(vec![1, 2])),
        );
        let outer = K::Dict(
            Box::new(K::SymbolV(vec!["a".into(), "b".into()])),
            Box::new(K::List(vec![inner.clone(), K::Int(3)])),
        );
        let text = to_text(&outer, 0);
        assert!(text.contains("a|"));
        assert!(text.contains("  x| 1"));
        assert!(text.contains("  y| 2"));
        assert!(text.contains("b| 3"));

        let capped = to_text(&inner, 1);
        assert!(capped.contains("x| 1"));
        assert!(capped.contains("..(1 more of 2 entries)"));
    }

    #[test]
    fn json_floats_nonfinite_are_null() {
        assert_eq!(json_frag(&K::Float(f64::NAN), 0), "null");
        assert_eq!(json_frag(&K::Float(1.5), 0), "1.5");
    }

    #[test]
    fn render_vector_max_rows_truncation() {
        let v = K::LongV((0..100).collect());
        let text = to_text(&v, 5);
        assert!(text.ends_with("..."));

        let json = to_json(&v, 5);
        assert_eq!(json, "[0,1,2,3,4]");
    }

    #[test]
    fn render_unicode_char_and_multibyte() {
        let c = K::Char('한');
        assert_eq!(scalar_to_string(&c), "한");
        assert_eq!(json_frag(&c, 0), "\"한\"");
    }

    #[test]
    fn render_scalar_symbol_and_list_symbols() {
        let sym = K::Symbol("trade".into());
        assert_eq!(to_text(&sym, 0), "`trade");

        let empty_sym = K::Symbol("".into());
        assert_eq!(to_text(&empty_sym, 0), "`");

        let list = K::List(vec![K::Int(1), K::Symbol("abc".into())]);
        let text = to_text(&list, 0);
        assert_eq!(text, "1\n`abc");
    }

    #[test]
    fn render_table_cell_with_newline_is_escaped() {
        let table = K::Table(Box::new(K::Dict(
            Box::new(K::SymbolV(vec!["id".into(), "msg".into()])),
            Box::new(K::List(vec![
                K::LongV(vec![1, 2]),
                K::List(vec![
                    K::CharV("hello\nworld".into()),
                    K::CharV("line1\r\nline2\ttab".into()),
                ]),
            ])),
        )));
        let text = to_text(&table, 0);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4);
        assert!(lines[2].contains("\"hello\\nworld\""));
        assert!(lines[3].contains("\"line1\\r\\nline2\\ttab\""));
    }

    #[test]
    fn compact_vec_byte_and_bool_vectors() {
        assert_eq!(compact_vec(&K::ByteV(vec![1, 2, 255]), 0), "0x0102ff");
        assert_eq!(compact_vec(&K::ByteV(vec![]), 0), "0x");
        assert_eq!(compact_vec(&K::ByteV(vec![10, 20, 30]), 2), "0x0a14...");
        assert_eq!(compact_vec(&K::BoolV(vec![true, false, true]), 0), "101b");
        assert_eq!(compact_vec(&K::BoolV(vec![]), 0), "0#0b");
        assert_eq!(
            compact_vec(&K::BoolV(vec![true, false, true, false]), 2),
            "10b..."
        );
    }

    #[test]
    fn csv_defuses_formula_text_but_not_numbers() {
        let table = K::Table(Box::new(K::Dict(
            Box::new(K::SymbolV(vec!["=h".into(), "n".into()])),
            Box::new(K::List(vec![
                K::SymbolV(vec!["=cmd|'/c calc'!A1".into(), "ok".into()]),
                K::LongV(vec![-5, 1]),
            ])),
        )));
        assert_eq!(to_csv(&table), "'=h,n\n'=cmd|'/c calc'!A1,-5\nok,1");
    }

    #[test]
    fn csv_string_column_and_temporal_nulls() {
        let table = K::Table(Box::new(K::Dict(
            Box::new(K::SymbolV(vec!["id".into(), "msg".into(), "d".into()])),
            Box::new(K::List(vec![
                K::IntV(vec![1, 2]),
                K::List(vec![K::CharV("hello".into()), K::CharV("a,b".into())]),
                K::DateV(vec![NULL_INT, 0]),
            ])),
        )));
        assert_eq!(to_csv(&table), "id,msg,d\n1,hello,\n2,\"a,b\",2000.01.01");
        assert_eq!(
            to_json(&table, 0),
            r#"[{"id":1,"msg":"hello","d":null},{"id":2,"msg":"a,b","d":"2000.01.01"}]"#
        );
        assert_eq!(to_text(&K::DateV(vec![NULL_INT, 0]), 0), "0Nd 2000.01.01");
    }

    #[test]
    fn corrupt_table_mismatched_columns_does_not_panic() {
        let bad_table = K::Table(Box::new(K::Dict(
            Box::new(K::SymbolV(vec!["a".into(), "b".into()])),
            Box::new(K::List(vec![K::IntV(vec![1, 2])])),
        )));
        assert_eq!(to_json(&bad_table, 0), "null");
        assert_eq!(to_text(&bad_table, 0), "(table: 0 rows)");
    }
}
