//! kdb+ K object model, wire deserialization, and temporal formatting.

/// A deserialized q value. Only the types a CLI realistically encounters are
/// modelled; anything else surfaces as an explicit error.
#[derive(Clone)]
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
    Char(u8),
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
}

pub const NULL_SHORT: i16 = i16::MIN;
pub const NULL_INT: i32 = i32::MIN;
pub const NULL_LONG: i64 = i64::MIN;

/// Cursor over a response payload.
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
    le: bool,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8], le: bool) -> Self {
        Reader { buf, pos: 0, le }
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
        let s = String::from_utf8_lossy(&self.buf[start..self.pos]).into_owned();
        self.pos += 1; // skip null
        Ok(s)
    }

    /// vector header: attribute byte + i32 length
    fn vhdr(&mut self) -> Result<usize, String> {
        let _attr = self.u8()?;
        let n = self.i32()?;
        if n < 0 {
            return Err("negative vector length".to_string());
        }
        Ok(n as usize)
    }

    /// Read one K object at the cursor.
    pub fn read(&mut self) -> Result<K, String> {
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
            -10 => Ok(K::Char(self.u8()?)),
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
                let n = self.vhdr()?;
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(self.read()?);
                }
                Ok(K::List(v))
            }
            1 => {
                let n = self.vhdr()?;
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(self.u8()? != 0);
                }
                Ok(K::BoolV(v))
            }
            2 => {
                let n = self.vhdr()?;
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(self.guid()?);
                }
                Ok(K::GuidV(v))
            }
            4 => {
                let n = self.vhdr()?;
                Ok(K::ByteV(self.take(n)?.to_vec()))
            }
            5 => {
                let n = self.vhdr()?;
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(self.i16()?);
                }
                Ok(K::ShortV(v))
            }
            6 => {
                let n = self.vhdr()?;
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(self.i32()?);
                }
                Ok(K::IntV(v))
            }
            7 => {
                let n = self.vhdr()?;
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(self.i64()?);
                }
                Ok(K::LongV(v))
            }
            8 => {
                let n = self.vhdr()?;
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(self.f32()?);
                }
                Ok(K::RealV(v))
            }
            9 => {
                let n = self.vhdr()?;
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(self.f64()?);
                }
                Ok(K::FloatV(v))
            }
            10 => {
                let n = self.vhdr()?;
                let bytes = self.take(n)?;
                Ok(K::CharV(String::from_utf8_lossy(bytes).into_owned()))
            }
            11 => {
                let n = self.vhdr()?;
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(self.sym()?);
                }
                Ok(K::SymbolV(v))
            }
            12 => {
                let n = self.vhdr()?;
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(self.i64()?);
                }
                Ok(K::TimestampV(v))
            }
            13 => {
                let n = self.vhdr()?;
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(self.i32()?);
                }
                Ok(K::MonthV(v))
            }
            14 => {
                let n = self.vhdr()?;
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(self.i32()?);
                }
                Ok(K::DateV(v))
            }
            15 => {
                let n = self.vhdr()?;
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(self.f64()?);
                }
                Ok(K::DatetimeV(v))
            }
            16 => {
                let n = self.vhdr()?;
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(self.i64()?);
                }
                Ok(K::TimespanV(v))
            }
            17 => {
                let n = self.vhdr()?;
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(self.i32()?);
                }
                Ok(K::MinuteV(v))
            }
            18 => {
                let n = self.vhdr()?;
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(self.i32()?);
                }
                Ok(K::SecondV(v))
            }
            19 => {
                let n = self.vhdr()?;
                let mut v = Vec::with_capacity(n);
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
                let _ = self.u8()?; // unary primitive id; 0 == (::)
                Ok(K::Null)
            }
            -128 => Err(format!("q error '{}", self.sym()?)),
            _ => Err(format!("unsupported q type code {}", t)),
        }
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
        K::CharV(v) => v.chars().count(),
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
pub fn at(k: &K, i: usize) -> K {
    match k {
        K::BoolV(v) => K::Bool(v[i]),
        K::GuidV(v) => K::Guid(v[i]),
        K::ByteV(v) => K::Byte(v[i]),
        K::ShortV(v) => K::Short(v[i]),
        K::IntV(v) => K::Int(v[i]),
        K::LongV(v) => K::Long(v[i]),
        K::RealV(v) => K::Real(v[i]),
        K::FloatV(v) => K::Float(v[i]),
        K::CharV(v) => K::Char(v.as_bytes()[i]),
        K::SymbolV(v) => K::Symbol(v[i].clone()),
        K::TimestampV(v) => K::Timestamp(v[i]),
        K::MonthV(v) => K::Month(v[i]),
        K::DateV(v) => K::Date(v[i]),
        K::DatetimeV(v) => K::Datetime(v[i]),
        K::TimespanV(v) => K::Timespan(v[i]),
        K::MinuteV(v) => K::Minute(v[i]),
        K::SecondV(v) => K::Second(v[i]),
        K::TimeV(v) => K::Time(v[i]),
        K::List(v) => v[i].clone(),
        other => other.clone(),
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
    let (y, m, dd) = civil(d as i64);
    format!("{:04}.{:02}.{:02}", y, m, dd)
}

pub fn fmt_month(m: i32) -> String {
    if m == NULL_INT {
        return String::new();
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
    let nanos = (v * 86_400_000_000_000.0).round() as i64;
    let (days, h, mi, s, ns) = hms_nanos(nanos);
    let (y, m, d) = civil(days);
    let ms = ns / 1_000_000;
    format!(
        "{:04}.{:02}.{:02}T{:02}:{:02}:{:02}.{:03}",
        y, m, d, h, mi, s, ms
    )
}

pub fn fmt_time(ms: i32) -> String {
    if ms == NULL_INT {
        return String::new();
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
    format!("{:02}:{:02}", m / 60, m % 60)
}

pub fn fmt_second(s: i32) -> String {
    if s == NULL_INT {
        return String::new();
    }
    format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
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
    let mut dst = vec![0u8; usize_total - 8];
    let mut aa = [0usize; 256];
    let mut s = 0usize; // write index into dst
    let mut p = 0usize;
    let mut d = 4usize; // read index into src (after the size word)
    let mut i: u32 = 0;
    let mut f: u32 = 0;
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
            let mut r = aa[src[d] as usize];
            d += 1;
            dst[s] = dst[r];
            s += 1;
            r += 1;
            dst[s] = dst[r];
            s += 1;
            r += 1;
            let n = src[d] as usize;
            d += 1;
            for _ in 0..n {
                dst[s] = dst[r];
                s += 1;
                r += 1;
            }
        } else {
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
