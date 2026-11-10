//! kdb+ K object model, wire deserialization, and temporal formatting.

/// Byte order of a kdb+ IPC message payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endian {
    Little,
    Big,
}

/// A deserialized q value. Only the types a CLI realistically encounters are
/// modelled; anything else surfaces as an explicit error.
#[derive(Debug, Clone, PartialEq)]
pub enum K {
    // atoms
    Bool(bool),
    Guid([u8; 16]),
    Byte(u8),
    Short(i16),
    Int(i32),
    Long(i64),
    Real(f32),
    Float(f64),
    Char(char),
    Symbol(String),
    Timestamp(i64),
    Month(i32),
    Date(i32),
    Datetime(f64),
    Timespan(i64),
    Minute(i32),
    Second(i32),
    Time(i32),
    // vectors
    BoolV(Vec<bool>),
    GuidV(Vec<[u8; 16]>),
    ByteV(Vec<u8>),
    ShortV(Vec<i16>),
    IntV(Vec<i32>),
    LongV(Vec<i64>),
    RealV(Vec<f32>),
    FloatV(Vec<f64>),
    CharV(String),
    SymbolV(Vec<String>),
    TimestampV(Vec<i64>),
    MonthV(Vec<i32>),
    DateV(Vec<i32>),
    DatetimeV(Vec<f64>),
    TimespanV(Vec<i64>),
    MinuteV(Vec<i32>),
    SecondV(Vec<i32>),
    TimeV(Vec<i32>),
    // compound
    List(Vec<K>),
    Dict(Box<K>, Box<K>), // keys, values
    Table(Box<K>),        // wraps the underlying Dict(SymbolV, List)
    Null,                 // (::) and friends
    Function(String),
    Projection(Vec<K>),
}

pub const NULL_SHORT: i16 = i16::MIN;
pub const NULL_INT: i32 = i32::MIN;
pub const NULL_LONG: i64 = i64::MIN;

pub const MAX_READ_DEPTH: usize = 128;

/// Largest response accepted, compressed or not. A server controls both the
/// length header and the compression ratio (a few MB can expand ~120x), so this
/// bounds what a hostile or wrong endpoint can make the client allocate.
/// Default 256 MB; raise with `Q_CLI_MAX_RESPONSE_MB`.
#[must_use]
pub fn max_message_len() -> usize {
    std::env::var("Q_CLI_MAX_RESPONSE_MB")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&mb| mb > 0)
        .unwrap_or(256)
        .saturating_mul(1024 * 1024)
}

#[inline]
fn safe_cap(n: usize) -> usize {
    n.min(65536)
}

/// Cursor over a response payload.
#[derive(Debug, Clone)]
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
    endian: Endian,
    depth: usize,
}

impl<'a> Reader<'a> {
    #[must_use]
    pub fn new(buf: &'a [u8], endian: Endian) -> Self {
        Self {
            buf,
            pos: 0,
            endian,
            depth: 0,
        }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let Some(end) = self.pos.checked_add(n) else {
            return Err("unexpected end of message".to_string());
        };
        if end > self.buf.len() {
            return Err("unexpected end of message".to_string());
        }
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn i8(&mut self) -> Result<i8, String> {
        Ok(self.u8()? as i8)
    }

    fn i16(&mut self) -> Result<i16, String> {
        let b = self.take(2)?;
        let a = [b[0], b[1]];
        Ok(match self.endian {
            Endian::Little => i16::from_le_bytes(a),
            Endian::Big => i16::from_be_bytes(a),
        })
    }

    fn i32(&mut self) -> Result<i32, String> {
        let b = self.take(4)?;
        let a = [b[0], b[1], b[2], b[3]];
        Ok(match self.endian {
            Endian::Little => i32::from_le_bytes(a),
            Endian::Big => i32::from_be_bytes(a),
        })
    }

    fn i64(&mut self) -> Result<i64, String> {
        let b = self.take(8)?;
        let a = [b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]];
        Ok(match self.endian {
            Endian::Little => i64::from_le_bytes(a),
            Endian::Big => i64::from_be_bytes(a),
        })
    }

    fn f32(&mut self) -> Result<f32, String> {
        Ok(f32::from_bits(self.i32()? as u32))
    }

    fn f64(&mut self) -> Result<f64, String> {
        Ok(f64::from_bits(self.i64()? as u64))
    }

    fn guid(&mut self) -> Result<[u8; 16], String> {
        let b = self.take(16)?;
        let mut g = [0u8; 16];
        g.copy_from_slice(b);
        Ok(g)
    }

    fn sym(&mut self) -> Result<String, String> {
        let start = self.pos;
        while self.pos < self.buf.len() && self.buf[self.pos] != 0 {
            self.pos += 1;
        }
        if self.pos >= self.buf.len() {
            return Err("unterminated symbol in response".to_string());
        }
        let s = String::from_utf8_lossy(&self.buf[start..self.pos]).into_owned();
        self.pos += 1; // skip null
        Ok(s)
    }

    /// vector header: attribute byte + i32 length, bounded by remaining buffer.
    fn vhdr(&mut self, elem_size: usize) -> Result<usize, String> {
        let attr = self.u8()?;
        if attr > 4 {
            return Err(format!(
                "invalid vector attribute byte 0x{attr:02x} (expected 0..=4)"
            ));
        }
        let n = self.i32()?;
        if n < 0 {
            return Err("negative vector length".to_string());
        }
        let count = n as usize;
        let remaining = self.buf.len().saturating_sub(self.pos);
        if count.saturating_mul(elem_size) > remaining {
            return Err(format!(
                "vector length {count} exceeds remaining payload ({remaining} bytes)"
            ));
        }
        Ok(count)
    }

    /// Read one K object at the cursor.
    pub fn read(&mut self) -> Result<K, String> {
        if self.depth >= MAX_READ_DEPTH {
            return Err(format!(
                "max deserialization recursion depth exceeded ({MAX_READ_DEPTH})"
            ));
        }
        self.depth += 1;
        let res = self.read_impl();
        self.depth -= 1;
        res
    }

    fn read_impl(&mut self) -> Result<K, String> {
        let t = self.i8()?;
        match t {
            // ---- atoms ----
            -1 => Ok(K::Bool(self.u8()? != 0)),
            -2 => Ok(K::Guid(self.guid()?)),
            -4 => Ok(K::Byte(self.u8()?)),
            -5 => Ok(K::Short(self.i16()?)),
            -6 => Ok(K::Int(self.i32()?)),
            -7 => Ok(K::Long(self.i64()?)),
            -8 => Ok(K::Real(self.f32()?)),
            -9 => Ok(K::Float(self.f64()?)),
            -10 => Ok(K::Char(self.u8()? as char)),
            -11 => Ok(K::Symbol(self.sym()?)),
            -12 => Ok(K::Timestamp(self.i64()?)),
            -13 => Ok(K::Month(self.i32()?)),
            -14 => Ok(K::Date(self.i32()?)),
            -15 => Ok(K::Datetime(self.f64()?)),
            -16 => Ok(K::Timespan(self.i64()?)),
            -17 => Ok(K::Minute(self.i32()?)),
            -18 => Ok(K::Second(self.i32()?)),
            -19 => Ok(K::Time(self.i32()?)),
            // ---- vectors ----
            0 => {
                let n = self.vhdr(1)?;
                let mut v = Vec::with_capacity(safe_cap(n));
                for _ in 0..n {
                    v.push(self.read()?);
                }
                Ok(K::List(v))
            }
            1 => {
                let n = self.vhdr(1)?;
                let mut v = Vec::with_capacity(safe_cap(n));
                for _ in 0..n {
                    v.push(self.u8()? != 0);
                }
                Ok(K::BoolV(v))
            }
            2 => {
                let n = self.vhdr(16)?;
                let mut v = Vec::with_capacity(safe_cap(n));
                for _ in 0..n {
                    v.push(self.guid()?);
                }
                Ok(K::GuidV(v))
            }
            4 => {
                let n = self.vhdr(1)?;
                Ok(K::ByteV(self.take(n)?.to_vec()))
            }
            5 => {
                let n = self.vhdr(2)?;
                let mut v = Vec::with_capacity(safe_cap(n));
                for _ in 0..n {
                    v.push(self.i16()?);
                }
                Ok(K::ShortV(v))
            }
            6 => {
                let n = self.vhdr(4)?;
                let mut v = Vec::with_capacity(safe_cap(n));
                for _ in 0..n {
                    v.push(self.i32()?);
                }
                Ok(K::IntV(v))
            }
            7 => {
                let n = self.vhdr(8)?;
                let mut v = Vec::with_capacity(safe_cap(n));
                for _ in 0..n {
                    v.push(self.i64()?);
                }
                Ok(K::LongV(v))
            }
            8 => {
                let n = self.vhdr(4)?;
                let mut v = Vec::with_capacity(safe_cap(n));
                for _ in 0..n {
                    v.push(self.f32()?);
                }
                Ok(K::RealV(v))
            }
            9 => {
                let n = self.vhdr(8)?;
                let mut v = Vec::with_capacity(safe_cap(n));
                for _ in 0..n {
                    v.push(self.f64()?);
                }
                Ok(K::FloatV(v))
            }
            10 => {
                let n = self.vhdr(1)?;
                let bytes = self.take(n)?;
                Ok(K::CharV(String::from_utf8_lossy(bytes).into_owned()))
            }
            11 => {
                let n = self.vhdr(1)?;
                let mut v = Vec::with_capacity(safe_cap(n));
                for _ in 0..n {
                    v.push(self.sym()?);
                }
                Ok(K::SymbolV(v))
            }
            12 => {
                let n = self.vhdr(8)?;
                let mut v = Vec::with_capacity(safe_cap(n));
                for _ in 0..n {
                    v.push(self.i64()?);
                }
                Ok(K::TimestampV(v))
            }
            13 => {
                let n = self.vhdr(4)?;
                let mut v = Vec::with_capacity(safe_cap(n));
                for _ in 0..n {
                    v.push(self.i32()?);
                }
                Ok(K::MonthV(v))
            }
            14 => {
                let n = self.vhdr(4)?;
                let mut v = Vec::with_capacity(safe_cap(n));
                for _ in 0..n {
                    v.push(self.i32()?);
                }
                Ok(K::DateV(v))
            }
            15 => {
                let n = self.vhdr(8)?;
                let mut v = Vec::with_capacity(safe_cap(n));
                for _ in 0..n {
                    v.push(self.f64()?);
                }
                Ok(K::DatetimeV(v))
            }
            16 => {
                let n = self.vhdr(8)?;
                let mut v = Vec::with_capacity(safe_cap(n));
                for _ in 0..n {
                    v.push(self.i64()?);
                }
                Ok(K::TimespanV(v))
            }
            17 => {
                let n = self.vhdr(4)?;
                let mut v = Vec::with_capacity(safe_cap(n));
                for _ in 0..n {
                    v.push(self.i32()?);
                }
                Ok(K::MinuteV(v))
            }
            18 => {
                let n = self.vhdr(4)?;
                let mut v = Vec::with_capacity(safe_cap(n));
                for _ in 0..n {
                    v.push(self.i32()?);
                }
                Ok(K::SecondV(v))
            }
            19 => {
                let n = self.vhdr(4)?;
                let mut v = Vec::with_capacity(safe_cap(n));
                for _ in 0..n {
                    v.push(self.i32()?);
                }
                Ok(K::TimeV(v))
            }
            // ---- compound ----
            98 => {
                let attr = self.u8()?;
                if attr > 4 {
                    return Err(format!(
                        "invalid table attribute byte 0x{attr:02x} (expected 0..=4)"
                    ));
                }
                let dict = self.read()?;
                if let K::Dict(ref keys, ref vals) = dict {
                    if let (K::SymbolV(names), K::List(cols)) = (&**keys, &**vals) {
                        if names.len() != cols.len() {
                            return Err(format!(
                                "corrupt table: {} column names but {} column vectors",
                                names.len(),
                                cols.len()
                            ));
                        }
                        return Ok(K::Table(Box::new(dict)));
                    }
                }
                Err("corrupt table: expected dictionary of symbol names to column list".to_string())
            }
            99 => {
                let keys = self.read()?;
                let vals = self.read()?;
                Ok(K::Dict(Box::new(keys), Box::new(vals)))
            }
            100 => {
                // lambda: namespace symbol + char-vector body; surface the source as Function
                let _ns = self.sym()?;
                match self.read()? {
                    K::CharV(s) => Ok(K::Function(s)),
                    other => Ok(other),
                }
            }
            101 => {
                let op = self.u8()?;
                if op == 0 {
                    Ok(K::Null)
                } else {
                    Ok(K::Function(unary_op_name(op)))
                }
            }
            102 => {
                let op = self.u8()?;
                Ok(K::Function(binary_op_name(op)))
            }
            103 => {
                let op = self.u8()?;
                Ok(K::Function(iter_name(op)))
            }
            104 => {
                let n = self.i32()?;
                if n < 0 || (n as usize) > self.buf.len().saturating_sub(self.pos) {
                    return Err("invalid projection length".to_string());
                }
                let mut v = Vec::with_capacity(safe_cap(n as usize));
                for _ in 0..n {
                    v.push(self.read()?);
                }
                Ok(K::Projection(v))
            }
            105 => {
                let n = self.i32()?;
                if n < 0 || (n as usize) > self.buf.len().saturating_sub(self.pos) {
                    return Err("invalid composition length".to_string());
                }
                let mut v = Vec::with_capacity(safe_cap(n as usize));
                for _ in 0..n {
                    v.push(self.read()?);
                }
                Ok(K::Projection(v))
            }
            106..=111 => {
                let inner = self.read()?;
                let adv = match t {
                    106 => "'",
                    107 => "/",
                    108 => "\\",
                    109 => "':",
                    110 => "/:",
                    111 => "\\:",
                    _ => "",
                };
                match inner {
                    K::Function(s) => Ok(K::Function(format!("{s}{adv}"))),
                    other => Ok(K::Projection(vec![other, K::Function(adv.to_string())])),
                }
            }
            112 => {
                let inner = self.read()?;
                match inner {
                    K::Function(s) => Ok(K::Function(format!("2: {s}"))),
                    other => Ok(K::Projection(vec![K::Function("2:".to_string()), other])),
                }
            }
            -128 => Err(format!("q error '{}'", self.sym()?)),
            _ => Err(format!("unsupported q type code {t}")),
        }
    }
}

fn unary_op_name(op: u8) -> String {
    match op {
        1 => "flip".into(),
        2 => "neg".into(),
        3 => "first".into(),
        4 => "reciprocal".into(),
        5 => "where".into(),
        6 => "reverse".into(),
        7 => "iasc".into(),
        8 => "idesc".into(),
        9 => "group".into(),
        10 => "not".into(),
        11 => "key".into(),
        12 => "enlist".into(),
        13 => "null".into(),
        14 => "count".into(),
        15 => "floor".into(),
        16 => "string".into(),
        17 => "distinct".into(),
        18 => "type".into(),
        19 => "value".into(),
        _ => format!("unary#{op}"),
    }
}

fn binary_op_name(op: u8) -> String {
    match op {
        0 => ":".into(),
        1 => "+".into(),
        2 => "-".into(),
        3 => "*".into(),
        4 => "%".into(),
        5 => "&".into(),
        6 => "|".into(),
        7 => "<".into(),
        8 => ">".into(),
        9 => "=".into(),
        10 => "<>".into(),
        11 => ">=".into(),
        12 => "<=".into(),
        _ => format!("op#{op}"),
    }
}

fn iter_name(op: u8) -> String {
    match op {
        0 => "'".into(),
        1 => "/".into(),
        2 => "\\".into(),
        3 => "':".into(),
        4 => "/:".into(),
        5 => "\\:".into(),
        _ => format!("iter#{op}"),
    }
}

/// Length of a vector/list/table column-ish K (rows for tables).
#[must_use]
pub fn len_of(k: &K) -> usize {
    match k {
        K::BoolV(v) => v.len(),
        K::GuidV(v) => v.len(),
        K::ByteV(v) => v.len(),
        K::ShortV(v) => v.len(),
        K::IntV(v) => v.len(),
        K::LongV(v) => v.len(),
        K::RealV(v) => v.len(),
        K::FloatV(v) => v.len(),
        K::CharV(v) => {
            if v.is_ascii() {
                v.len()
            } else {
                v.chars().count()
            }
        }
        K::SymbolV(v) => v.len(),
        K::TimestampV(v) => v.len(),
        K::MonthV(v) => v.len(),
        K::DateV(v) => v.len(),
        K::DatetimeV(v) => v.len(),
        K::TimespanV(v) => v.len(),
        K::MinuteV(v) => v.len(),
        K::SecondV(v) => v.len(),
        K::TimeV(v) => v.len(),
        K::List(v) => v.len(),
        _ => 1,
    }
}

/// Extract element `i` of a vector/list K as a scalar K (clones).
/// Bounds-checked: returns K::Null if `i` is out of bounds, preventing panics on ragged tables.
#[must_use]
pub fn at(k: &K, i: usize) -> K {
    match k {
        K::BoolV(v) => v.get(i).copied().map(K::Bool).unwrap_or(K::Null),
        K::GuidV(v) => v.get(i).copied().map(K::Guid).unwrap_or(K::Null),
        K::ByteV(v) => v.get(i).copied().map(K::Byte).unwrap_or(K::Null),
        K::ShortV(v) => v.get(i).copied().map(K::Short).unwrap_or(K::Null),
        K::IntV(v) => v.get(i).copied().map(K::Int).unwrap_or(K::Null),
        K::LongV(v) => v.get(i).copied().map(K::Long).unwrap_or(K::Null),
        K::RealV(v) => v.get(i).copied().map(K::Real).unwrap_or(K::Null),
        K::FloatV(v) => v.get(i).copied().map(K::Float).unwrap_or(K::Null),
        K::CharV(v) => {
            if v.is_ascii() {
                v.as_bytes()
                    .get(i)
                    .copied()
                    .map(|b| K::Char(b as char))
                    .unwrap_or(K::Null)
            } else {
                v.chars().nth(i).map(K::Char).unwrap_or(K::Null)
            }
        }
        K::SymbolV(v) => v.get(i).cloned().map(K::Symbol).unwrap_or(K::Null),
        K::TimestampV(v) => v.get(i).copied().map(K::Timestamp).unwrap_or(K::Null),
        K::MonthV(v) => v.get(i).copied().map(K::Month).unwrap_or(K::Null),
        K::DateV(v) => v.get(i).copied().map(K::Date).unwrap_or(K::Null),
        K::DatetimeV(v) => v.get(i).copied().map(K::Datetime).unwrap_or(K::Null),
        K::TimespanV(v) => v.get(i).copied().map(K::Timespan).unwrap_or(K::Null),
        K::MinuteV(v) => v.get(i).copied().map(K::Minute).unwrap_or(K::Null),
        K::SecondV(v) => v.get(i).copied().map(K::Second).unwrap_or(K::Null),
        K::TimeV(v) => v.get(i).copied().map(K::Time).unwrap_or(K::Null),
        K::List(v) => v.get(i).cloned().unwrap_or(K::Null),
        other => {
            if i == 0 {
                other.clone()
            } else {
                K::Null
            }
        }
    }
}

// ----------------------------------------------------------------------------
// Temporal formatting. kdb+ epoch is 2000.01.01; 10957 days after the 1970 epoch.
// ----------------------------------------------------------------------------

const DAYS_2000_FROM_1970: i64 = 10957;

/// (year, month, day) from days-since-2000.01.01 (Howard Hinnant's algorithm).
fn civil(days_since_2000: i64) -> (i64, u32, u32) {
    let clamped = days_since_2000.clamp(-100_000_000, 100_000_000);
    let z = clamped + DAYS_2000_FROM_1970 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[must_use]
pub fn fmt_date(d: i32) -> String {
    if d == NULL_INT {
        return String::new();
    }
    if d == i32::MAX {
        return "0Wd".into();
    }
    if d == -i32::MAX {
        return "-0Wd".into();
    }
    let (y, m, dd) = civil(d as i64);
    format!("{y:04}.{m:02}.{dd:02}")
}

#[must_use]
pub fn fmt_month(m: i32) -> String {
    if m == NULL_INT {
        return String::new();
    }
    if m == i32::MAX {
        return "0Wm".into();
    }
    if m == -i32::MAX {
        return "-0Wm".into();
    }
    let y = 2000 + m.div_euclid(12);
    let mon = m.rem_euclid(12) + 1;
    format!("{y:04}.{mon:02}")
}

fn hms_nanos(mut nanos: i64) -> (i64, u32, u32, u32, u32) {
    let days = nanos.div_euclid(86_400_000_000_000);
    nanos = nanos.rem_euclid(86_400_000_000_000);
    let h = (nanos / 3_600_000_000_000) as u32;
    nanos %= 3_600_000_000_000;
    let mi = (nanos / 60_000_000_000) as u32;
    nanos %= 60_000_000_000;
    let s = (nanos / 1_000_000_000) as u32;
    let ns = (nanos % 1_000_000_000) as u32;
    (days, h, mi, s, ns)
}

#[must_use]
pub fn fmt_timestamp(v: i64) -> String {
    if v == NULL_LONG {
        return String::new();
    }
    if v == i64::MAX {
        return "0Wp".into();
    }
    if v == -i64::MAX {
        return "-0Wp".into();
    }
    let (days, h, mi, s, ns) = hms_nanos(v);
    let (y, m, d) = civil(days);
    format!("{y:04}.{m:02}.{d:02}D{h:02}:{mi:02}:{s:02}.{ns:09}")
}

#[must_use]
pub fn fmt_timespan(v: i64) -> String {
    if v == NULL_LONG {
        return String::new();
    }
    if v == i64::MAX {
        return "0Wn".into();
    }
    if v == -i64::MAX {
        return "-0Wn".into();
    }
    let sign = if v < 0 { "-" } else { "" };
    let (days, h, mi, s, ns) = hms_nanos(v.saturating_abs());
    format!("{sign}{days}D{h:02}:{mi:02}:{s:02}.{ns:09}")
}

#[must_use]
pub fn fmt_datetime(v: f64) -> String {
    if v.is_nan() {
        return String::new();
    }
    if v.is_infinite() || !(-100_000_000.0..=100_000_000.0).contains(&v) {
        return (if v.is_sign_positive() { "0Wz" } else { "-0Wz" }).into();
    }
    let total_ms = (v * 86_400_000.0).round() as i64;
    let days = total_ms.div_euclid(86_400_000);
    let ms = total_ms.rem_euclid(86_400_000) as u64;
    let (y, m, d) = civil(days);
    let h = ms / 3_600_000;
    let mi = (ms % 3_600_000) / 60_000;
    let s = (ms % 60_000) / 1000;
    let rem_ms = ms % 1000;
    format!("{y:04}.{m:02}.{d:02}T{h:02}:{mi:02}:{s:02}.{rem_ms:03}")
}

#[must_use]
pub fn fmt_time(ms: i32) -> String {
    if ms == NULL_INT {
        return String::new();
    }
    if ms == i32::MAX {
        return "0Wt".into();
    }
    if ms == -i32::MAX {
        return "-0Wt".into();
    }
    let sign = if ms < 0 { "-" } else { "" };
    let v = ms.unsigned_abs();
    let h = v / 3_600_000;
    let mi = (v / 60_000) % 60;
    let s = (v / 1000) % 60;
    let rem = v % 1000;
    format!("{sign}{h:02}:{mi:02}:{s:02}.{rem:03}")
}

#[must_use]
pub fn fmt_minute(m: i32) -> String {
    if m == NULL_INT {
        return String::new();
    }
    if m == i32::MAX {
        return "0Wu".into();
    }
    if m == -i32::MAX {
        return "-0Wu".into();
    }
    let sign = if m < 0 { "-" } else { "" };
    let v = m.unsigned_abs();
    let h = v / 60;
    let mi = v % 60;
    format!("{sign}{h:02}:{mi:02}")
}

#[must_use]
pub fn fmt_second(s: i32) -> String {
    if s == NULL_INT {
        return String::new();
    }
    if s == i32::MAX {
        return "0Wv".into();
    }
    if s == -i32::MAX {
        return "-0Wv".into();
    }
    let sign = if s < 0 { "-" } else { "" };
    let v = s.unsigned_abs();
    let h = v / 3600;
    let mi = (v / 60) % 60;
    let sec = v % 60;
    format!("{sign}{h:02}:{mi:02}:{sec:02}")
}

#[must_use]
pub fn fmt_guid(g: &[u8; 16]) -> String {
    format!(
        concat!(
            "{:02x}{:02x}{:02x}{:02x}-",
            "{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-",
            "{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}"
        ),
        g[0],
        g[1],
        g[2],
        g[3],
        g[4],
        g[5],
        g[6],
        g[7],
        g[8],
        g[9],
        g[10],
        g[11],
        g[12],
        g[13],
        g[14],
        g[15]
    )
}

// ----------------------------------------------------------------------------
// kdb+ IPC decompression (port of the canonical c.java algorithm).
// Only exercised for non-loopback responses > 2000 bytes.
// ----------------------------------------------------------------------------

pub fn decompress(src: &[u8], endian: Endian) -> Result<Vec<u8>, String> {
    // first 4 bytes of a compressed payload = uncompressed size (incl. 8B header)
    if src.len() < 4 {
        return Err("compressed payload too short".to_string());
    }
    let size_bytes = [src[0], src[1], src[2], src[3]];
    let usize_total = match endian {
        Endian::Little => u32::from_le_bytes(size_bytes),
        Endian::Big => u32::from_be_bytes(size_bytes),
    } as usize;
    if usize_total < 8 {
        return Err("bad uncompressed size".to_string());
    }
    let max = max_message_len();
    if usize_total > max {
        return Err(format!(
            "uncompressed size {usize_total} exceeds the {} MB limit (Q_CLI_MAX_RESPONSE_MB)",
            max / (1024 * 1024)
        ));
    }
    let mut dst = vec![0u8; usize_total - 8];
    let mut hash_table = [0usize; 256];
    let mut dst_pos = 0usize; // write index into dst
    let mut hash_pos = 0usize;
    let mut src_pos = 4usize; // read index into src (after the size word)
    let mut bit_mask: u32 = 0;
    let mut ctrl_byte: u32 = 0;
    // Every src/dst index below is bounds-checked: a corrupt or malicious
    // response must surface as Err (-> exit 4), never an out-of-bounds panic
    // (which, with panic=abort, would kill the process and lose the exit code).
    while dst_pos < dst.len() {
        if bit_mask == 0 {
            if src_pos >= src.len() {
                return Err("decompress overrun".to_string());
            }
            ctrl_byte = src[src_pos] as u32;
            src_pos += 1;
            bit_mask = 1;
        }
        let hash_end;
        if (ctrl_byte & bit_mask) != 0 {
            // back-reference: src[src_pos] selects the copy source, src[src_pos+1] the extra
            // length; the run copies 2 + that many bytes from earlier in dst. Only its first
            // 2 bytes are hashed (c.java `p=s1+=n`), or later references resolve wrongly.
            if src_pos + 1 >= src.len() {
                return Err("decompress overrun (back-reference)".to_string());
            }
            let mut ref_pos = hash_table[src[src_pos] as usize];
            if ref_pos >= dst_pos {
                return Err("decompress invalid back-reference before written offset".to_string());
            }
            let mut remaining = src[src_pos + 1] as usize + 2; // 2 + extra length
            src_pos += 2;
            hash_end = dst_pos + 2;
            while remaining > 0 {
                if dst_pos >= dst.len() || ref_pos >= dst.len() {
                    return Err("decompress output overrun".to_string());
                }
                dst[dst_pos] = dst[ref_pos]; // byte-by-byte so overlapping (LZ) runs work
                dst_pos += 1;
                ref_pos += 1;
                remaining -= 1;
            }
        } else {
            if src_pos >= src.len() {
                return Err("decompress overrun (literal)".to_string());
            }
            dst[dst_pos] = src[src_pos];
            dst_pos += 1;
            src_pos += 1;
            hash_end = dst_pos;
        }
        while hash_pos + 1 < hash_end {
            hash_table[(dst[hash_pos] as usize) ^ (dst[hash_pos + 1] as usize)] = hash_pos;
            hash_pos += 1;
        }
        if (ctrl_byte & bit_mask) != 0 {
            hash_pos = dst_pos;
        }
        bit_mask <<= 1;
        if bit_mask == 256 {
            bit_mask = 0;
        }
    }
    Ok(dst)
}

#[cfg(test)]
mod tests {
    use super::*;

    // kdb+ epoch is 2000.01.01 == day 0.
    #[test]
    fn temporal_epoch_and_boundaries() {
        assert_eq!(fmt_date(0), "2000.01.01");
        assert_eq!(fmt_date(30), "2000.01.31");
        assert_eq!(fmt_date(31), "2000.02.01");
        assert_eq!(fmt_date(-1), "1999.12.31");
        assert_eq!(fmt_month(0), "2000.01");
        assert_eq!(fmt_month(13), "2001.02");
        assert_eq!(fmt_timestamp(0), "2000.01.01D00:00:00.000000000");
        assert_eq!(fmt_time(3_600_000), "01:00:00.000");
        assert_eq!(fmt_minute(90), "01:30");
        assert_eq!(fmt_second(3661), "01:01:01");
    }

    #[test]
    fn temporal_nulls_render_empty() {
        assert_eq!(fmt_date(NULL_INT), "");
        assert_eq!(fmt_timestamp(NULL_LONG), "");
        assert_eq!(fmt_time(NULL_INT), "");
    }

    #[test]
    fn guid_formats_canonically() {
        let g = [
            0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
            0xcd, 0xef,
        ];
        assert_eq!(fmt_guid(&g), "01234567-89ab-cdef-0123-456789abcdef");
    }

    // A stream with no back-references: one 0x00 control byte then literal bytes.
    #[test]
    fn decompress_literal_only_roundtrip() {
        // [usize_total=11 (=8 header + 3 data)][control 0x00][1,2,3]
        let src = [11u8, 0, 0, 0, 0x00, 1, 2, 3];
        assert_eq!(decompress(&src, Endian::Little).unwrap(), vec![1u8, 2, 3]);
    }

    // A truncated back-reference must return Err, never panic (panic=abort would
    // crash the process and lose the structured exit code).
    #[test]
    fn decompress_malformed_is_err_not_panic() {
        let src = [100u8, 0, 0, 0, 0x01, 5]; // control bit set but no ref bytes
        assert!(decompress(&src, Endian::Little).is_err());
        assert!(decompress(&[1u8], Endian::Little).is_err()); // too short for the size word
    }

    #[test]
    fn decompress_invalid_backreference_before_written_offset_is_err() {
        // [usize_total=12 (=8 header + 4 data)][control 0x01][src[d]=0, src[d+1]=0]
        // At start dst_pos=0, hash_table[0]=0, so ref_pos=0 >= dst_pos(0), rejected.
        let src = [12u8, 0, 0, 0, 0x01, 0, 0];
        let res = decompress(&src, Endian::Little);
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("invalid back-reference"));
    }

    #[test]
    fn decompress_backreference_roundtrip() {
        // 2 literals [10, 20] -> hash_table[10 ^ 20 = 30] = 0.
        // Then 1 back-ref (bit 2 -> ctrl = 0x04) from hash 30 with extra_len=2 (total 4 bytes).
        // Output should be [10, 20, 10, 20, 10, 20] (6 bytes -> usize_total = 14).
        let src = [14u8, 0, 0, 0, 0x04, 10, 20, 30, 2];
        assert_eq!(
            decompress(&src, Endian::Little).unwrap(),
            vec![10u8, 20, 10, 20, 10, 20]
        );
    }

    #[test]
    fn decompress_kdb_payload_with_extended_backreferences() {
        // Captured from kdb+ 4.1: -18!"\n" sv {x," = ",string y}'[600#("alpha";"beta";"gamma");til 600]
        // Back-references with extra length are followed by more back-references, so the
        // hash table must skip the extra bytes exactly as c.java does.
        let msg = include_bytes!("../testdata/compressed-lines.bin");
        assert_eq!(msg[2], 1, "fixture must be a compressed message");
        let body = decompress(&msg[8..], Endian::Little).unwrap();
        let names = ["alpha", "beta", "gamma"];
        let text = (0..600)
            .map(|i| format!("{} = {}", names[i % 3], i))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(&body[..2], &[10u8, 0]);
        assert_eq!(
            u32::from_le_bytes(body[2..6].try_into().unwrap()) as usize,
            text.len()
        );
        assert_eq!(&body[6..], text.as_bytes());
    }

    #[test]
    fn big_endian_and_compound_deserialization() {
        // Big-endian Int atom (-6 = 250u8) with value 42
        let mut buf = vec![250u8];
        buf.extend_from_slice(&42i32.to_be_bytes());
        assert_eq!(Reader::new(&buf, Endian::Big).read().unwrap(), K::Int(42));

        // q error response (-128 = 128u8) with symbol "type\0"
        let err_buf = [128u8, b't', b'y', b'p', b'e', 0];
        let err = Reader::new(&err_buf, Endian::Little).read().unwrap_err();
        assert_eq!(err, "q error 'type'");
    }

    #[test]
    fn datetime_rounding_and_extreme_bounds() {
        // 0.9999999999 days rounds up to 1.0 day -> 2000.01.02T00:00:00.000
        assert_eq!(fmt_datetime(0.9999999999), "2000.01.02T00:00:00.000");
        assert_eq!(fmt_datetime(1e30), "0Wz");
        assert_eq!(fmt_datetime(-1e30), "-0Wz");
    }

    #[test]
    fn decompress_over_1gb_is_err() {
        // Size word claiming 2GB
        let src = [0x00, 0x00, 0x00, 0x80, 0x00, 1, 2, 3];
        assert!(decompress(&src, Endian::Little).is_err());
    }

    #[test]
    fn sym_unterminated_returns_err() {
        // -11 (symbol atom), followed by "abc" with NO null terminator
        let buf = [245u8, b'a', b'b', b'c'];
        assert!(Reader::new(&buf, Endian::Little).read().is_err());
    }

    #[test]
    fn vhdr_exceeding_buffer_returns_err_not_oom() {
        // Type 1 (bool vector): count=i32::MAX but payload has only 4 bytes
        let mut buf = vec![1u8, 0u8];
        buf.extend_from_slice(&i32::MAX.to_le_bytes());
        buf.extend_from_slice(&[1, 0, 1, 0]);
        assert!(Reader::new(&buf, Endian::Little).read().is_err());
    }

    #[test]
    fn vhdr_invalid_attribute_returns_err() {
        // Type 1 (bool vector): type=1, invalid attr=99, count=1, payload=[1]
        let mut buf = vec![1u8, 99u8];
        buf.extend_from_slice(&1i32.to_le_bytes());
        buf.push(1u8);
        let err = Reader::new(&buf, Endian::Little).read();
        assert!(err.is_err());
        assert!(err
            .unwrap_err()
            .contains("invalid vector attribute byte 0x63"));
    }

    #[test]
    fn recursion_depth_limit_prevents_stack_overflow() {
        // Construct 150 nested type 0 (generic list of 1 element)
        let mut buf = Vec::new();
        for _ in 0..150 {
            buf.push(0u8); // type 0: generic list
            buf.push(0u8); // attr
            buf.extend_from_slice(&1i32.to_le_bytes()); // count = 1
        }
        buf.push(245u8); // -11 (symbol)
        buf.push(0u8); // empty symbol `
        let err = Reader::new(&buf, Endian::Little).read();
        assert!(err.is_err());
        assert!(err
            .unwrap_err()
            .contains("max deserialization recursion depth exceeded"));
    }

    #[test]
    fn negative_minute_and_second_formatting() {
        assert_eq!(fmt_minute(-90), "-01:30");
        assert_eq!(fmt_minute(-5), "-00:05");
        assert_eq!(fmt_second(-65), "-00:01:05");
        assert_eq!(fmt_second(-3665), "-01:01:05");
    }

    #[test]
    fn at_bounds_checking_prevents_panics() {
        let v = K::LongV(vec![10, 20]);
        assert_eq!(at(&v, 0), K::Long(10));
        assert_eq!(at(&v, 1), K::Long(20));
        assert_eq!(at(&v, 2), K::Null); // OOB yields K::Null instead of panic
        assert_eq!(at(&v, 100), K::Null);

        let cv = K::CharV("가나다".into());
        assert_eq!(at(&cv, 0), K::Char('가'));
        assert_eq!(at(&cv, 1), K::Char('나'));
        assert_eq!(at(&cv, 2), K::Char('다'));
        assert_eq!(at(&cv, 3), K::Null);

        // Scalar atoms return K::Null when index > 0
        assert_eq!(at(&K::Long(10), 0), K::Long(10));
        assert_eq!(at(&K::Long(10), 1), K::Null);
        assert_eq!(at(&K::Symbol("x".into()), 1), K::Null);
    }

    #[test]
    fn temporal_infinity_formatting() {
        assert_eq!(fmt_timestamp(i64::MAX), "0Wp");
        assert_eq!(fmt_timestamp(-i64::MAX), "-0Wp");
        assert_eq!(fmt_date(i32::MAX), "0Wd");
        assert_eq!(fmt_date(-i32::MAX), "-0Wd");
        assert_eq!(fmt_time(i32::MAX), "0Wt");
        assert_eq!(fmt_timespan(i64::MAX), "0Wn");
        assert_eq!(fmt_datetime(f64::INFINITY), "0Wz");
        assert_eq!(fmt_datetime(f64::NEG_INFINITY), "-0Wz");
        assert_eq!(fmt_minute(i32::MAX), "0Wu");
        assert_eq!(fmt_second(i32::MAX), "0Wv");
    }

    #[test]
    fn unary_op_names_and_lambda_deserialization() {
        assert_eq!(unary_op_name(1), "flip");
        assert_eq!(unary_op_name(4), "reciprocal");
        assert_eq!(unary_op_name(5), "where");
        assert_eq!(unary_op_name(11), "key");
        assert_eq!(unary_op_name(14), "count");
        assert_eq!(unary_op_name(16), "string");
        assert_eq!(unary_op_name(19), "value");

        // Type 100 (lambda): empty ns symbol `\0` + type 10 char vector "{x+1}"
        let mut buf = vec![100u8, 0u8, 10u8, 0u8];
        buf.extend_from_slice(&5i32.to_le_bytes());
        buf.extend_from_slice(b"{x+1}");
        assert_eq!(
            Reader::new(&buf, Endian::Little).read().unwrap(),
            K::Function("{x+1}".into())
        );
    }
}
