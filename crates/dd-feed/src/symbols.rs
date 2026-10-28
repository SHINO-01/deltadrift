//! Venue symbol → dense [`InstrumentId`] lookup.
//!
//! Built once on the cold path; lookups on the hot path are a hash, a mask
//! and (almost always) one comparison — no allocation, no string hashing
//! library, no pointer chasing. Symbols are packed into a `u128` (≤ 16 bytes).

use dd_core::{InstrumentId, MAX_INSTRUMENTS};

#[derive(Debug, Clone)]
pub struct SymbolTable {
    keys: Box<[u128]>,
    ids: Box<[u32]>,
    mask: usize,
    names: Vec<String>,
}

#[inline(always)]
fn pack(sym: &[u8]) -> Option<u128> {
    if sym.is_empty() || sym.len() > 16 {
        return None;
    }
    let mut b = [0u8; 16];
    b[..sym.len()].copy_from_slice(sym);
    Some(u128::from_le_bytes(b))
}

#[inline(always)]
fn hash(k: u128) -> usize {
    let x = (k as u64) ^ ((k >> 64) as u64).rotate_left(29);
    (x.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 32) as usize
}

impl SymbolTable {
    /// Instrument ids are assigned in order: `symbols[i]` → `InstrumentId(i)`.
    pub fn new<S: AsRef<str>>(symbols: &[S]) -> Result<Self, String> {
        if symbols.is_empty() || symbols.len() > MAX_INSTRUMENTS {
            return Err(format!("need 1..={MAX_INSTRUMENTS} symbols, got {}", symbols.len()));
        }
        let cap = (symbols.len() * 4).next_power_of_two().max(16);
        let mut keys = vec![0u128; cap].into_boxed_slice();
        let mut ids = vec![0u32; cap].into_boxed_slice();
        for (i, s) in symbols.iter().enumerate() {
            let s = s.as_ref();
            let k = pack(s.as_bytes()).ok_or_else(|| format!("symbol `{s}` must be 1..=16 bytes"))?;
            let mut slot = hash(k) & (cap - 1);
            while keys[slot] != 0 {
                if keys[slot] == k {
                    return Err(format!("duplicate symbol `{s}`"));
                }
                slot = (slot + 1) & (cap - 1);
            }
            keys[slot] = k;
            ids[slot] = i as u32;
        }
        Ok(Self { keys, ids, mask: cap - 1, names: symbols.iter().map(|s| s.as_ref().to_string()).collect() })
    }

    #[inline]
    pub fn lookup(&self, sym: &[u8]) -> Option<InstrumentId> {
        let k = pack(sym)?;
        let mut slot = hash(k) & self.mask;
        loop {
            match self.keys[slot] {
                0 => return None,
                x if x == k => return Some(InstrumentId(self.ids[slot])),
                _ => slot = (slot + 1) & self.mask,
            }
        }
    }

    pub fn name(&self, id: InstrumentId) -> &str {
        self.names.get(id.idx()).map_or("?", String::as_str)
    }

    pub fn symbols(&self) -> &[String] {
        &self.names
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_and_errors() {
        let syms: Vec<String> = (0..200).map(|i| format!("SYM{i}USDT")).collect();
        let t = SymbolTable::new(&syms).unwrap();
        for (i, s) in syms.iter().enumerate() {
            assert_eq!(t.lookup(s.as_bytes()), Some(InstrumentId(i as u32)));
        }
        assert_eq!(t.lookup(b"NOPE"), None);
        assert_eq!(t.lookup(b""), None);
        assert_eq!(t.lookup(b"THIS_IS_WAY_TOO_LONG"), None);
        assert!(SymbolTable::new(&["A", "A"]).is_err());
        assert!(SymbolTable::new(&["THIS_IS_WAY_TOO_LONG"]).is_err());
        assert_eq!(t.name(InstrumentId(3)), "SYM3USDT");
    }
}
