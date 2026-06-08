//! Render a deserialized `K` value as readable text or as JSON.
//! Row caps take a `max` argument; `max == 0` means unlimited.

use crate::k::*;

// ============================================================================
// Shared scalar formatting
// ============================================================================

fn fmt_float(v: f64) -> String {
    if v.is_nan() {
        String::new()
    } else if v.is_infinite() {
        if v > 0.0 { "0w".into() } else { "-0w".into() }
    } else {
        format!("{}", v)
    }
}

/// Plain string for a single scalar (also used for table cells).
pub fn scalar_to_string(k: &K) -> String {
    match k {
        K::Bool(b) => (if *b { "1" } else { "0" }).into(),
        K::Byte(x) => format!("0x{:02x}", x),
        K::Short(v) => if *v == NULL_SHORT { String::new() } else { v.to_string() },
        K::Int(v) => if *v == NULL_INT { String::new() } else { v.to_string() },
        K::Long(v) => if *v == NULL_LONG { String::new() } else { v.to_string() },
        K::Real(v) => fmt_float(*v as f64),
        K::Float(v) => fmt_float(*v),
        K::Char(c) => (*c as char).to_string(),
        K::Symbol(s) => s.clone(),
        K::Timestamp(v) => fmt_timestamp(*v),
        K::Month(v) => fmt_month(*v),
        K::Date(v) => fmt_date(*v),
        K::Datetime(v) => fmt_datetime(*v),
        K::Timespan(v) => fmt_timespan(*v),
        K::Minute(v) => fmt_minute(*v),
        K::Second(v) => fmt_second(*v),
        K::Time(v) => fmt_time(*v),
        K::Guid(g) => fmt_guid(g),
        K::Null => String::new(),
        // nested table/dict in a scalar slot -> compact summary (no recursion)
        K::Table(_) => format!("(table: {} rows)", table_rows(k).unwrap_or(0)),
        K::Dict(_, _) => "(dict)".into(),
        other => compact_vec(other),
    }
}

/// Compact one-line form for a vector (used inside lists / nested cells).
fn compact_vec(k: &K) -> String {
    match k {
        K::CharV(s) => format!("\"{}\"", s),
        K::SymbolV(v) => v.iter().map(|s| format!("`{}", s)).collect::<String>(),
        K::List(v) => {
            let inner = v.iter().map(scalar_to_string).collect::<Vec<_>>().join(";");
            format!("({})", inner)
        }
        _ => {
            let n = len_of(k);
            (0..n)
                .map(|i| scalar_to_string(&at(k, i)))
                .collect::<Vec<_>>()
                .join(" ")
        }
    }
}

// ============================================================================
// Table extraction
// ============================================================================

fn is_table(k: &K) -> bool {
    matches!(k, K::Table(_))
}

fn dict_parts(d: &K) -> Option<(Vec<String>, Vec<K>)> {
    if let K::Dict(keys, vals) = d {
        if let (K::SymbolV(names), K::List(cols)) = (&**keys, &**vals) {
            return Some((names.clone(), cols.clone()));
        }
    }
    None
}

/// (column names, columns) for a table or keyed table.
fn table_parts(k: &K) -> Option<(Vec<String>, Vec<K>)> {
    match k {
        K::Table(d) => dict_parts(d),
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

/// Row count if `k` is a table or keyed table (used for truncation signalling).
pub fn table_rows(k: &K) -> Option<usize> {
    table_parts(k).map(|(_, cols)| cols.first().map(len_of).unwrap_or(0))
}

fn cap(nrows: usize, max: usize) -> usize {
    if max == 0 { nrows } else { nrows.min(max) }
}

// ============================================================================
// Text rendering
// ============================================================================

pub fn to_text(k: &K, max: usize) -> String {
    if let Some((names, cols)) = table_parts(k) {
        return render_table(&names, &cols, max);
    }
    match k {
        K::Dict(keys, vals) => render_dict(keys, vals, max),
        K::CharV(s) => s.clone(),
        K::List(v) => v
            .iter()
            .map(scalar_to_string)
            .collect::<Vec<_>>()
            .join("\n"),
        K::Null => "::".into(),
        atom_or_vec => {
            if len_of(atom_or_vec) > 1 {
                compact_vec(atom_or_vec)
            } else {
                scalar_to_string(atom_or_vec)
            }
        }
    }
}

fn render_table(names: &[String], cols: &[K], max: usize) -> String {
    let nrows = cols.first().map(len_of).unwrap_or(0);
    let shown = cap(nrows, max);

    let mut widths: Vec<usize> = names.iter().map(|n| n.len()).collect();
    let mut cells: Vec<Vec<String>> = Vec::with_capacity(cols.len());
    for (ci, col) in cols.iter().enumerate() {
        let mut colcells = Vec::with_capacity(shown);
        for i in 0..shown {
            let s = scalar_to_string(&at(col, i));
            widths[ci] = widths[ci].max(s.len());
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
    let total: usize = widths.iter().sum::<usize>() + names.len().saturating_sub(1);
    out.push_str(&"-".repeat(total));
    out.push('\n');
    for i in 0..shown {
        for ci in 0..cols.len() {
            if ci > 0 {
                out.push(' ');
            }
            out.push_str(&pad(&cells[ci][i], widths[ci]));
        }
        out.push('\n');
    }
    if nrows > shown {
        out.push_str(&format!("..({} more of {} rows)\n", nrows - shown, nrows));
    }
    out.trim_end().to_string()
}

fn pad(s: &str, w: usize) -> String {
    if s.len() >= w {
        s.to_string()
    } else {
        format!("{}{}", s, " ".repeat(w - s.len()))
    }
}

/// Dict as `key| value` lines; a table-valued entry is rendered as an indented
/// sub-table block (so e.g. `describe`'s columns/sample show in full).
fn render_dict(keys: &K, vals: &K, max: usize) -> String {
    let n = len_of(keys);
    let kw = (0..n)
        .map(|i| scalar_to_string(&at(keys, i)).len())
        .max()
        .unwrap_or(0);
    let mut lines: Vec<String> = Vec::new();
    for i in 0..n {
        let kstr = scalar_to_string(&at(keys, i));
        let v = at(vals, i);
        if table_parts(&v).is_some() {
            lines.push(format!("{}|", kstr));
            for l in to_text(&v, max).lines() {
                lines.push(format!("  {}", l));
            }
        } else {
            lines.push(format!("{}| {}", pad(&kstr, kw), scalar_to_string(&v)));
        }
    }
    lines.join("\n")
}

// ============================================================================
// CSV rendering (tables only; non-tables fall back to text). Uncapped.
// ============================================================================

pub fn to_csv(k: &K) -> String {
    let (names, cols) = match table_parts(k) {
        Some(p) => p,
        None => return to_text(k, 0),
    };
    let nrows = cols.first().map(len_of).unwrap_or(0);
    let mut out = String::new();
    out.push_str(
        &names
            .iter()
            .map(|n| csv_field(n))
            .collect::<Vec<_>>()
            .join(","),
    );
    out.push('\n');
    for i in 0..nrows {
        let row: Vec<String> = cols
            .iter()
            .map(|c| csv_field(&scalar_to_string(&at(c, i))))
            .collect();
        out.push_str(&row.join(","));
        out.push('\n');
    }
    out.trim_end().to_string()
}

fn csv_field(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

// ============================================================================
// JSON rendering
// ============================================================================

pub fn to_json(k: &K, max: usize) -> String {
    if let Some((names, cols)) = table_parts(k) {
        return json_rows(&names, &cols, max);
    }
    json_frag(k)
}

fn json_rows(names: &[String], cols: &[K], max: usize) -> String {
    let nrows = cap(cols.first().map(len_of).unwrap_or(0), max);
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
            out.push_str(&json_frag(&at(&cols[ci], i)));
        }
        out.push('}');
    }
    out.push(']');
    out
}

fn json_frag(k: &K) -> String {
    match k {
        K::Bool(b) => (if *b { "true" } else { "false" }).into(),
        K::Byte(x) => x.to_string(),
        K::Short(v) => num_or_null(*v == NULL_SHORT, *v as i64),
        K::Int(v) => num_or_null(*v == NULL_INT, *v as i64),
        K::Long(v) => num_or_null(*v == NULL_LONG, *v),
        K::Real(v) => float_json(*v as f64),
        K::Float(v) => float_json(*v),
        K::Char(c) => json_string(&(*c as char).to_string()),
        K::Symbol(s) => json_string(s),
        K::Timestamp(v) => json_string(&fmt_timestamp(*v)),
        K::Month(v) => json_string(&fmt_month(*v)),
        K::Date(v) => json_string(&fmt_date(*v)),
        K::Datetime(v) => json_string(&fmt_datetime(*v)),
        K::Timespan(v) => json_string(&fmt_timespan(*v)),
        K::Minute(v) => json_string(&fmt_minute(*v)),
        K::Second(v) => json_string(&fmt_second(*v)),
        K::Time(v) => json_string(&fmt_time(*v)),
        K::Guid(g) => json_string(&fmt_guid(g)),
        K::CharV(s) => json_string(s),
        K::SymbolV(v) => {
            let items: Vec<String> = v.iter().map(|s| json_string(s)).collect();
            format!("[{}]", items.join(","))
        }
        K::List(v) => {
            let items: Vec<String> = v.iter().map(json_frag).collect();
            format!("[{}]", items.join(","))
        }
        K::Dict(keys, vals) => json_dict(keys, vals),
        K::Table(_) => {
            if let Some((names, cols)) = table_parts(k) {
                json_rows(&names, &cols, 0)
            } else {
                "null".into()
            }
        }
        K::Null => "null".into(),
        other => {
            let n = len_of(other);
            let items: Vec<String> = (0..n).map(|i| json_frag(&at(other, i))).collect();
            format!("[{}]", items.join(","))
        }
    }
}

fn json_dict(keys: &K, vals: &K) -> String {
    if let K::SymbolV(names) = keys {
        let n = names.len();
        let mut out = String::from("{");
        for i in 0..n {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&json_string(&names[i]));
            out.push(':');
            out.push_str(&json_frag(&at(vals, i)));
        }
        out.push('}');
        out
    } else if is_table(keys) && is_table(vals) {
        if let Some((names, cols)) =
            table_parts(&K::Dict(Box::new(keys.clone()), Box::new(vals.clone())))
        {
            return json_rows(&names, &cols, 0);
        }
        "null".into()
    } else {
        format!(
            "{{\"keys\":{},\"values\":{}}}",
            json_frag(keys),
            json_frag(vals)
        )
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
        format!("{}", v)
    } else {
        "null".into()
    }
}

/// JSON-escape a string (public so error output can reuse it).
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
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
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
    fn json_respects_max_rows() {
        assert_eq!(to_json(&sample_table(), 1), r#"[{"a":1,"b":"x"}]"#);
    }

    #[test]
    fn csv_field_quotes_when_needed() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("she said \"hi\""), "\"she said \"\"hi\"\"\"");
    }

    #[test]
    fn scalar_nulls_and_floats() {
        assert_eq!(scalar_to_string(&K::Int(NULL_INT)), "");
        assert_eq!(scalar_to_string(&K::Long(42)), "42");
        assert_eq!(fmt_float(f64::INFINITY), "0w");
        assert_eq!(fmt_float(f64::NAN), "");
    }

    #[test]
    fn json_floats_nonfinite_are_null() {
        assert_eq!(json_frag(&K::Float(f64::NAN)), "null");
        assert_eq!(json_frag(&K::Float(1.5)), "1.5");
    }
}
