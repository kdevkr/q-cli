//! The fixed q expressions the CLI builds and sends, kept in one place so the
//! q-side logic is reviewable apart from the dispatch/IO plumbing.

/// Server health snapshot: version, pid, port, open handles, timer + memory.
pub const INFO_Q: &str = "(`version`pid`port`handles`timer!(.z.K;.z.i;first system\"p\";count .z.W;first system\"t\")),`used`heap`peak#.Q.w[]";

/// Whole-DB overview in ONE round-trip: per table, partitioned? + partition
/// field + row count + column count, razed server-side into a single table.
/// `.Q.pf` is trapped (unset on a non-partitioned process). `count` on a
/// partitioned table uses cached partition counts — it never forces a full load.
/// Each table is probed under its OWN `@[...]` trap, so one unreadable table
/// (corrupt/locked partition) yields null counts instead of failing the whole
/// command. Locals avoid q reserved names (`i`); the `columns` column avoids the
/// `cols` keyword — either would raise `'assign`.
pub const SCHEMA_Q: &str = "{ts:tables[]; pf:@[{.Q.pf};::;`]; f:{[pf;t] @[{[pf;t] tt:value t; qp:.Q.qp tt; (qp~1b;$[qp~1b;pf;`];count tt;count cols tt)}[pf;]; t; (0b;`;0N;0N)]}; r:f[pf;] each ts; ([] table:ts; partitioned:r[;0]; partition:r[;1]; rows:r[;2]; columns:r[;3])}[]";

/// One-shot table profile: name, partitioned?, partition field, rows, columns
/// (meta), and a small sample. Sample is skipped for partitioned tables so we
/// never trigger a full multi-partition load. Locals stay inside the lambda.
pub fn describe_q(t: &str) -> String {
    format!(
        "{{[t] tt:value t; p:.Q.qp tt; `name`partitioned`partition`rows`columns`sample!(t;p;$[p~1b;.Q.pf;`];count tt;0!meta tt;$[p~1b;();3 sublist 0!tt])}}`{}",
        t
    )
}

/// List functions in a namespace (root by default), with arity. `system "f .."`
/// yields the short names; we re-qualify them so `value value` can reach each
/// lambda's param list. Arity is trapped to a null int for anything non-lambda.
pub fn functions_q(ns: &str) -> String {
    let ns = ns.trim_start_matches('.');
    let nsym = if ns.is_empty() { "`".to_string() } else { format!("`{}", ns) };
    const BODY: &str = "{[ns] fs:system $[ns~`;\"f\";\"f .\",string ns]; pre:$[ns~`;\"\";\".\",(string ns),\".\"]; full:`$pre,/:string each fs; ([] func:fs; args:{@[{count (value value x)1};x;0Ni]} each full)}";
    format!("{}[{}]", BODY, nsym)
}

/// Wrap an expression so the server times it and returns `ms` + result `count`.
pub fn time_q(expr: &str) -> String {
    format!(
        "t:.z.p;r:value\"{}\";`ms`count!(`float$(.z.p-t)%1000000;count r)",
        q_escape(expr)
    )
}

/// Escape a q expression so it can be embedded inside a q string literal.
pub fn q_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => o.push_str("\\\\"),
            '"' => o.push_str("\\\""),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c => o.push(c),
        }
    }
    o
}
