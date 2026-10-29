//! Minimal Protocol Buffers (proto2) wire-format codec.
//!
//! Hand-rolled instead of generated code (prost) because we need ~a dozen
//! messages, want zero-allocation decoding of the hot `SpotEvent` path, and
//! avoid a `protoc` build dependency. Field numbers come from Spotware's
//! official `.proto` files (github.com/spotware/openapi-proto-messages).

/// Wire types.
const VARINT: u8 = 0;
const FIXED64: u8 = 1;
const LEN: u8 = 2;
const FIXED32: u8 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Fixed64(u64),
    Fixed32(u32),
}

impl<'a> Value<'a> {
    #[inline(always)]
    pub fn u64(self) -> Option<u64> {
        match self {
            Value::Varint(v) | Value::Fixed64(v) => Some(v),
            Value::Fixed32(v) => Some(u64::from(v)),
            Value::Bytes(_) => None,
        }
    }
    /// proto `int64`/`int32` (two's complement varint, not zigzag).
    #[inline(always)]
    pub fn i64(self) -> Option<i64> {
        self.u64().map(|v| v as i64)
    }
    #[inline(always)]
    pub fn bytes(self) -> Option<&'a [u8]> {
        match self {
            Value::Bytes(b) => Some(b),
            _ => None,
        }
    }
    pub fn str(self) -> Option<&'a str> {
        std::str::from_utf8(self.bytes()?).ok()
    }
}

/// Zero-copy iterator over `(field_number, value)` pairs. Stops (yielding
/// `Err`) on malformed input.
#[derive(Debug, Clone)]
pub struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf }
    }

    #[inline(always)]
    fn varint(&mut self) -> Option<u64> {
        let mut v = 0u64;
        for i in 0..10 {
            let (&b, rest) = self.buf.split_first()?;
            self.buf = rest;
            v |= u64::from(b & 0x7F) << (7 * i);
            if b & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }

    #[inline(always)]
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.buf.len() < n {
            return None;
        }
        let (a, b) = self.buf.split_at(n);
        self.buf = b;
        Some(a)
    }
}

impl<'a> Iterator for Reader<'a> {
    type Item = Result<(u32, Value<'a>), ()>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        if self.buf.is_empty() {
            return None;
        }
        let r = (|| {
            let key = self.varint()?;
            let field = u32::try_from(key >> 3).ok()?;
            let v = match (key & 7) as u8 {
                VARINT => Value::Varint(self.varint()?),
                FIXED64 => Value::Fixed64(u64::from_le_bytes(self.take(8)?.try_into().ok()?)),
                LEN => {
                    let n = usize::try_from(self.varint()?).ok()?;
                    Value::Bytes(self.take(n)?)
                }
                FIXED32 => Value::Fixed32(u32::from_le_bytes(self.take(4)?.try_into().ok()?)),
                _ => return None,
            };
            Some((field, v))
        })();
        match r {
            Some(x) => Some(Ok(x)),
            None => {
                self.buf = &[];
                Some(Err(()))
            }
        }
    }
}

/// Appending encoder (cold path: requests are built once per session step).
#[derive(Debug, Default)]
pub struct Writer {
    pub buf: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Self::default()
    }

    fn varint(&mut self, mut v: u64) {
        while v >= 0x80 {
            self.buf.push((v as u8) | 0x80);
            v >>= 7;
        }
        self.buf.push(v as u8);
    }

    fn key(&mut self, field: u32, wt: u8) {
        self.varint((u64::from(field) << 3) | u64::from(wt));
    }

    pub fn uint(&mut self, field: u32, v: u64) -> &mut Self {
        self.key(field, VARINT);
        self.varint(v);
        self
    }

    pub fn int(&mut self, field: u32, v: i64) -> &mut Self {
        self.uint(field, v as u64)
    }

    pub fn bool(&mut self, field: u32, v: bool) -> &mut Self {
        self.uint(field, u64::from(v))
    }

    pub fn bytes(&mut self, field: u32, v: &[u8]) -> &mut Self {
        self.key(field, LEN);
        self.varint(v.len() as u64);
        self.buf.extend_from_slice(v);
        self
    }

    pub fn str(&mut self, field: u32, v: &str) -> &mut Self {
        self.bytes(field, v.as_bytes())
    }

    pub fn finish(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_all_types() {
        let mut w = Writer::new();
        w.uint(1, 2131).int(2, -5).str(3, "EURUSD").bool(4, true).uint(5, u64::MAX);
        let buf = w.finish();
        let got: Vec<_> = Reader::new(&buf).map(Result::unwrap).collect();
        assert_eq!(got[0], (1, Value::Varint(2131)));
        assert_eq!(got[1].1.i64(), Some(-5));
        assert_eq!(got[2].1.str(), Some("EURUSD"));
        assert_eq!(got[3], (4, Value::Varint(1)));
        assert_eq!(got[4].1.u64(), Some(u64::MAX));
    }

    #[test]
    fn skips_fixed_and_rejects_truncation() {
        // field 9 fixed64, field 10 fixed32
        let mut buf = vec![(9 << 3) | 1];
        buf.extend_from_slice(&7u64.to_le_bytes());
        buf.push((10 << 3) | 5);
        buf.extend_from_slice(&3u32.to_le_bytes());
        let got: Vec<_> = Reader::new(&buf).map(Result::unwrap).collect();
        assert_eq!(got, [(9, Value::Fixed64(7)), (10, Value::Fixed32(3))]);
        let truncated = [(2 << 3) | 2, 5, b'a'];
        assert_eq!(Reader::new(&truncated).next(), Some(Err(())));
    }
}
