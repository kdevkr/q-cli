//! Render a deserialized `K` value as readable text or as JSON.

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

const ROW_CAP: usize = 50;

// ============================================================================
// Text rendering
// ============================================================================

pub fn to_text(k: &K) -> String {
    if let Some((names, cols)) = table_parts(k) {
        return render_table(&names, &cols);
    }
    match k {
        K::Dict(keys, vals) => render_dict(keys, vals),
        K::CharV(s) => s.clone(),
        K::List(v) => v
            .iter()
            .map(scalar_to_string)
            .collect::<Vec<_>>()
            .join("\n"),
        K::Null => "::".into(),
        atom_or_vec => {
            // vectors -> compact line; atoms -> their scalar form
            if len_of(atom_or_vec) > 1 {
                compact_vec(atom_or_vec)
            } else {
                scalar_to_string(atom_or_vec)
            }
        }
    }
}

fn render_table(names: &[String], cols: &[K]) -> String {
    let nrows = cols.first().map(len_of).unwrap_or(0);
    let shown = nrows.min(ROW_CAP);

    // build cell strings column-by-column
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
    // header
    for (ci, name) in names.iter().enumerate() {
        if ci > 0 {
            out.push(' ');
        }
        out.push_str(&pad(name, widths[ci]));
    }
    out.push('\n');
    // separator
    let total: usize = widths.iter().sum::<usize>() + names.len().saturating_sub(1);
    out.push_str(&"-".repeat(total));
    out.push('\n');
    // rows
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

fn render_dict(keys: &K, vals: &K) -> String {
    let n = len_of(keys);
    let mut rows: Vec<(String, String)> = Vec::with_capacity(n);
    let mut kw = 0;
    for i in 0..n {
        let ks = scalar_to_string(&at(keys, i));
        let vs = scalar_to_string(&at(vals, i));
        kw = kw.max(ks.len());
        rows.push((ks, vs));
    }
    rows.iter()
        .map(|(kk, vv)| format!("{}| {}", pad(kk, kw), vv))
        .collect::<Vec<_>>()
        .join("\n")
}

// ============================================================================
// CSV rendering (tables only; non-tables fall back to text). Uncapped — CSV is
// meant for export/piping, so we emit every row.
// ============================================================================

pub fn to_csv(k: &K) -> String {
    let (names, cols) = match table_parts(k) {
        Some(p) => p,
        None => return to_text(k),
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

pub fn to_json(k: &K) -> String {
    // tables (and keyed tables) -> array of row objects
    if let Some((names, cols)) = table_parts(k) {
        return json_rows(&names, &cols);
    }
    json_frag(k)
}

fn json_rows(names: &[String], cols: &[K]) -> String {
    let nrows = cols.first().map(len_of).unwrap_or(0);
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
                json_rows(&names, &cols)
            } else {
                "null".into()
            }
        }
        K::Null => "null".into(),
        // remaining typed vectors -> array of element fragments
        other => {
            let n = len_of(other);
            let items: Vec<String> = (0..n).map(|i| json_frag(&at(other, i))).collect();
            format!("[{}]", items.join(","))
        }
    }
}

fn json_dict(keys: &K, vals: &K) -> String {
    // keyed table handled by to_json/json_frag(Table); here: plain dict
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
        if let Some((names, cols)) = table_parts(&K::Dict(
            Box::new(keys.clone()),
            Box::new(vals.clone()),
        )) {
            return json_rows(&names, &cols);
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

fn json_string(s: &str) -> String {
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
