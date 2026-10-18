//! kdb+ K object model, wire deserialization, and temporal formatting.

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

#[inline]
fn safe_cap(n: usize) -> usize {
    n.min(65536)
}

/// Cursor over a response payload.
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
    le: bool,
    depth: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8], le: bool) -> Self {
        Reader { buf, pos: 0, le, depth: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        if self.pos + n > self.buf.len() {
            return Err("unexpected end of message".to_string());
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
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
        Ok(if self.le {
            i16::from_le_bytes(a)
        } else {
            i16::from_be_bytes(a)
        })
    }
    fn i32(&mut self) -> Result<i32, String> {
        let b = self.take(4)?;
        let a = [b[0], b[1], b[2], b[3]];
        Ok(if self.le {
            i32::from_le_bytes(a)
        } else {
            i32::from_be_bytes(a)
        })
    }
    fn i64(&mut self) -> Result<i64, String> {
        let b = self.take(8)?;
        let a = [b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]];
        Ok(if self.le {
            i64::from_le_bytes(a)
        } else {
            i64::from_be_bytes(a)
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
        let _attr = self.u8()?;
        let n = self.i32()?;
        if n < 0 {
            return Err("negative vector length".to_string());
        }
        let count = n as usize;
        let remaining = self.buf.len().saturating_sub(self.pos);
        if count.saturating_mul(elem_size) > remaining {
            return Err(format!(
                "vector length {} exceeds remaining payload ({} bytes)",
                count, remaining
            ));
        }
        Ok(count)
    }

    /// Read one K object at the cursor.
    pub fn read(&mut self) -> Result<K, String> {
        if self.depth >= MAX_READ_DEPTH {
            return Err(format!(
                "max deserialization recursion depth exceeded ({})",
                MAX_READ_DEPTH
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
                let _attr = self.u8()?;
                let dict = self.read()?;
                Ok(K::Table(Box::new(dict)))
            }
            99 => {
                let keys = self.read()?;
                let vals = self.read()?;
                Ok(K::Dict(Box::new(keys), Box::new(vals)))
            }
            100 => {
                // lambda: namespace symbol + char-vector body; surface the source
                let _ns = self.sym()?;
                self.read()
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
                    K::Function(s) => Ok(K::Function(format!("{}{}", s, adv))),
                    other => Ok(K::Projection(vec![other, K::Function(adv.to_string())])),
                }
            }
            112 => {
                let inner = self.read()?;
                match inner {
                    K::Function(s) => Ok(K::Function(format!("2: {}", s))),
                    other => Ok(K::Projection(vec![K::Function("2:".to_string()), other])),
                }
            }
            -128 => Err(format!("q error '{}", self.sym()?)),
            _ => Err(format!("unsupported q type code {}", t)),
        }
    }
}

fn unary_op_name(op: u8) -> String {
    match op {
        1 => "flip".into(),
        2 => "neg".into(),
        3 => "first".into(),
        4 => "where".into(),
        5 => "reverse".into(),
        6 => "asc".into(),
        7 => "desc".into(),
        8 => "group".into(),
        9 => "not".into(),
        10 => "distinct".into(),
        11 => "enlist".into(),
        12 => "null".into(),
        13 => "count".into(),
        14 => "floor".into(),
        15 => "string".into(),
        _ => format!("unary#{}", op),
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
        _ => format!("op#{}", op),
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
        _ => format!("iter#{}", op),
    }
}

/// Length of a vector/list/table column-ish K (rows for tables).
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
                v.as_bytes().get(i).copied().map(|b| K::Char(b as char)).unwrap_or(K::Null)
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
        other => if i == 0 { other.clone() } else { K::Null },
    }
}

// ----------------------------------------------------------------------------
// Temporal formatting. kdb+ epoch is 2000.01.01; 10957 days after the 1970 epoch.
// ----------------------------------------------------------------------------

const DAYS_2000_FROM_1970: i64 = 10957;

/// (year, month, day) from days-since-2000.01.01 (Howard Hinnant's algorithm).
fn civil(days_since_2000: i64) -> (i64, u32, u32) {
    let z = days_since_2000 + DAYS_2000_FROM_1970 + 719468;
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
    format!("{:04}.{:02}.{:02}", y, m, dd)
}

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
    format!("{:04}.{:02}", y, mon)
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
    format!(
        "{:04}.{:02}.{:02}D{:02}:{:02}:{:02}.{:09}",
        y, m, d, h, mi, s, ns
    )
}

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
    let neg = v < 0;
    let (days, h, mi, s, ns) = hms_nanos(v.abs());
    format!(
        "{}{}D{:02}:{:02}:{:02}.{:09}",
        if neg { "-" } else { "" },
        days,
        h,
        mi,
        s,
        ns
    )
}

pub fn fmt_datetime(v: f64) -> String {
    if v.is_nan() {
        return String::new();
    }
    if v.is_infinite() {
        return (if v.is_sign_positive() { "0Wz" } else { "-0Wz" }).into();
    }
    let days = v.floor() as i64;
    let frac = (v - v.floor()).abs();
    let (y, m, d) = civil(days);
    let ms = ((frac * 86_400_000.0).round() as u64).min(86_399_999);
    let h = ms / 3_600_000;
    let mi = (ms % 3_600_000) / 60_000;
    let s = (ms % 60_000) / 1000;
    let rem_ms = ms % 1000;
    format!(
        "{:04}.{:02}.{:02}T{:02}:{:02}:{:02}.{:03}",
        y, m, d, h, mi, s, rem_ms
    )
}

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
    let neg = ms < 0;
    let v = ms.abs();
    format!(
        "{}{:02}:{:02}:{:02}.{:03}",
        if neg { "-" } else { "" },
        v / 3_600_000,
        (v / 60_000) % 60,
        (v / 1000) % 60,
        v % 1000
    )
}

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
    let neg = m < 0;
    let v = m.abs();
    format!("{}{:02}:{:02}", if neg { "-" } else { "" }, v / 60, v % 60)
}

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
    let neg = s < 0;
    let v = s.abs();
    format!(
        "{}{:02}:{:02}:{:02}",
        if neg { "-" } else { "" },
        v / 3600,
        (v / 60) % 60,
        v % 60
    )
}

pub fn fmt_guid(g: &[u8; 16]) -> String {
    let h: String = g.iter().map(|b| format!("{:02x}", b)).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

// ----------------------------------------------------------------------------
// kdb+ IPC decompression (port of the canonical c.java algorithm).
// Only exercised for non-loopback responses > 2000 bytes.
// ----------------------------------------------------------------------------

pub fn decompress(src: &[u8], le: bool) -> Result<Vec<u8>, String> {
    // first 4 bytes of a compressed payload = uncompressed size (incl. 8B header)
    if src.len() < 4 {
        return Err("compressed payload too short".to_string());
    }
    let usize_total = if le {
        u32::from_le_bytes([src[0], src[1], src[2], src[3]])
    } else {
        u32::from_be_bytes([src[0], src[1], src[2], src[3]])
    } as usize;
    if usize_total < 8 {
        return Err("bad uncompressed size".to_string());
    }
    const MAX_UNCOMPRESSED_SIZE: usize = 1024 * 1024 * 1024; // 1GB limit
    if usize_total > MAX_UNCOMPRESSED_SIZE {
        return Err(format!("uncompressed size {} exceeds 1GB limit", usize_total));
    }
    let mut dst = vec![0u8; usize_total - 8];
    let mut aa = [0usize; 256];
    let mut s = 0usize; // write index into dst
    let mut p = 0usize;
    let mut d = 4usize; // read index into src (after the size word)
    let mut i: u32 = 0;
    let mut f: u32 = 0;
    // Every src/dst index below is bounds-checked: a corrupt or malicious
    // response must surface as Err (-> exit 4), never an out-of-bounds panic
    // (which, with panic=abort, would kill the process and lose the exit code).
    while s < dst.len() {
        if i == 0 {
            if d >= src.len() {
                return Err("decompress overrun".to_string());
            }
            f = src[d] as u32;
            d += 1;
            i = 1;
        }
        if (f & i) != 0 {
            // back-reference: src[d] selects the copy source, src[d+1] the extra
            // length; the run copies 2 + that many bytes from earlier in dst.
            if d + 1 >= src.len() {
                return Err("decompress overrun (back-reference)".to_string());
            }
            let mut r = aa[src[d] as usize]; // copy source: a pointer back into dst
            if r >= s {
                return Err("decompress invalid back-reference before written offset".to_string());
            }
            let mut remaining = src[d + 1] as usize + 2; // 2 + extra length
            d += 2;
            while remaining > 0 {
                if s >= dst.len() || r >= dst.len() {
                    return Err("decompress output overrun".to_string());
                }
                dst[s] = dst[r]; // byte-by-byte so overlapping (LZ) runs work
                s += 1;
                r += 1;
                remaining -= 1;
            }
        } else {
            if d >= src.len() {
                return Err("decompress overrun (literal)".to_string());
            }
            dst[s] = src[d];
            s += 1;
            d += 1;
        }
        while p < s.saturating_sub(1) {
            aa[(dst[p] as usize) ^ (dst[p + 1] as usize)] = p;
            p += 1;
        }
        if (f & i) != 0 {
            p = s;
        }
        i <<= 1;
        if i == 256 {
            i = 0;
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
        assert_eq!(decompress(&src, true).unwrap(), vec![1u8, 2, 3]);
    }

    // A truncated back-reference must return Err, never panic (panic=abort would
    // crash the process and lose the structured exit code).
    #[test]
    fn decompress_malformed_is_err_not_panic() {
        let src = [100u8, 0, 0, 0, 0x01, 5]; // control bit set but no ref bytes
        assert!(decompress(&src, true).is_err());
        assert!(decompress(&[1u8], true).is_err()); // too short for the size word
    }

    #[test]
    fn decompress_invalid_backreference_before_written_offset_is_err() {
        // [usize_total=12 (=8 header + 4 data)][control 0x01 (back-ref first)][src[d]=0, src[d+1]=0]
        // At start s=0, aa[0]=0, so r=0 >= s(0), which must be rejected.
        let src = [12u8, 0, 0, 0, 0x01, 0, 0];
        let res = decompress(&src, true);
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("invalid back-reference"));
    }

    #[test]
    fn decompress_over_1gb_is_err() {
        // Size word claiming 2GB
        let src = [0x00, 0x00, 0x00, 0x80, 0x00, 1, 2, 3];
        assert!(decompress(&src, true).is_err());
    }

    #[test]
    fn sym_unterminated_returns_err() {
        // -11 (symbol atom), followed by "abc" with NO null terminator
        let buf = [245u8, b'a', b'b', b'c'];
        assert!(Reader::new(&buf, true).read().is_err());
    }

    #[test]
    fn vhdr_exceeding_buffer_returns_err_not_oom() {
        // Type 1 (bool vector): type=1, attr=0, count=0x7fffffff (2 billion) but payload has only 4 bytes
        let mut buf = vec![1u8, 0u8];
        buf.extend_from_slice(&i32::MAX.to_le_bytes());
        buf.extend_from_slice(&[1, 0, 1, 0]);
        assert!(Reader::new(&buf, true).read().is_err());
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
        let err = Reader::new(&buf, true).read();
        assert!(err.is_err());
        assert!(err.unwrap_err().contains("max deserialization recursion depth exceeded"));
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
}
