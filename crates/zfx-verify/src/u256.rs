//! Minimal unsigned 256-bit integer for targets and cumulative work.
//! Big-endian byte form is the consensus encoding (matches `uint256_be`).

use std::cmp::Ordering;

/// Limbs are little-endian: limbs[0] is least significant.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Debug)]
pub struct U256(pub [u64; 4]);

impl U256 {
    pub const ZERO: U256 = U256([0; 4]);
    pub const MAX: U256 = U256([u64::MAX; 4]);

    pub fn from_u64(v: u64) -> Self {
        U256([v, 0, 0, 0])
    }

    pub fn from_be_bytes(b: &[u8; 32]) -> Self {
        let mut l = [0u64; 4];
        for i in 0..4 {
            let mut w = [0u8; 8];
            w.copy_from_slice(&b[(3 - i) * 8..(4 - i) * 8]);
            l[i] = u64::from_be_bytes(w);
        }
        U256(l)
    }

    pub fn to_be_bytes(&self) -> [u8; 32] {
        let mut out = [0u8; 32];
        for i in 0..4 {
            out[(3 - i) * 8..(4 - i) * 8].copy_from_slice(&self.0[i].to_be_bytes());
        }
        out
    }

    pub fn is_zero(&self) -> bool {
        self.0 == [0; 4]
    }

    pub fn bits(&self) -> u32 {
        for i in (0..4).rev() {
            if self.0[i] != 0 {
                return 64 * i as u32 + (64 - self.0[i].leading_zeros());
            }
        }
        0
    }

    pub fn checked_add(&self, o: &U256) -> Option<U256> {
        let mut r = [0u64; 4];
        let mut carry = 0u128;
        for i in 0..4 {
            let s = self.0[i] as u128 + o.0[i] as u128 + carry;
            r[i] = s as u64;
            carry = s >> 64;
        }
        if carry != 0 {
            None
        } else {
            Some(U256(r))
        }
    }

    pub fn saturating_add(&self, o: &U256) -> U256 {
        self.checked_add(o).unwrap_or(U256::MAX)
    }

    pub fn sub(&self, o: &U256) -> U256 {
        let mut r = [0u64; 4];
        let mut borrow = 0i128;
        for i in 0..4 {
            let d = self.0[i] as i128 - o.0[i] as i128 - borrow;
            if d < 0 {
                r[i] = (d + (1i128 << 64)) as u64;
                borrow = 1;
            } else {
                r[i] = d as u64;
                borrow = 0;
            }
        }
        assert!(borrow == 0, "U256 underflow");
        U256(r)
    }

    /// self * m, saturating at MAX.
    pub fn saturating_mul_u64(&self, m: u64) -> U256 {
        let mut r = [0u64; 4];
        let mut carry = 0u128;
        for i in 0..4 {
            let p = self.0[i] as u128 * m as u128 + carry;
            r[i] = p as u64;
            carry = p >> 64;
        }
        if carry != 0 {
            U256::MAX
        } else {
            U256(r)
        }
    }

    pub fn div_u64(&self, d: u64) -> U256 {
        assert!(d != 0);
        let mut r = [0u64; 4];
        let mut rem = 0u128;
        for i in (0..4).rev() {
            let cur = (rem << 64) | self.0[i] as u128;
            r[i] = (cur / d as u128) as u64;
            rem = cur % d as u128;
        }
        U256(r)
    }

    pub fn shr(&self, n: u32) -> U256 {
        if n >= 256 {
            return U256::ZERO;
        }
        let limb = (n / 64) as usize;
        let bit = n % 64;
        let mut r = [0u64; 4];
        for i in 0..4 - limb {
            r[i] = self.0[i + limb] >> bit;
            if bit > 0 && i + limb + 1 < 4 {
                r[i] |= self.0[i + limb + 1] << (64 - bit);
            }
        }
        U256(r)
    }

    pub fn shl(&self, n: u32) -> U256 {
        if n >= 256 {
            return U256::ZERO;
        }
        let limb = (n / 64) as usize;
        let bit = n % 64;
        let mut r = [0u64; 4];
        for i in (limb..4).rev() {
            r[i] = self.0[i - limb] << bit;
            if bit > 0 && i - limb >= 1 {
                r[i] |= self.0[i - limb - 1] >> (64 - bit);
            }
        }
        U256(r)
    }

    /// Expected attempts to beat `target`: floor(2^256 / (target + 1)),
    /// computed as (~target / (target + 1)) + 1 (Bitcoin's GetBlockProof).
    pub fn work_for_target(target: &U256) -> U256 {
        if *target == U256::MAX {
            return U256::from_u64(1);
        }
        let not_t = U256([!target.0[0], !target.0[1], !target.0[2], !target.0[3]]);
        let t1 = target.checked_add(&U256::from_u64(1)).unwrap();
        not_t.div_u256(&t1).saturating_add(&U256::from_u64(1))
    }

    /// Long division (shift-subtract). Only used for work accounting.
    pub fn div_u256(&self, d: &U256) -> U256 {
        assert!(!d.is_zero());
        if self < d {
            return U256::ZERO;
        }
        let shift = self.bits() - d.bits();
        let mut q = U256::ZERO;
        let mut rem = *self;
        let mut dd = d.shl(shift);
        for s in (0..=shift).rev() {
            if rem >= dd {
                rem = rem.sub(&dd);
                q.0[(s / 64) as usize] |= 1u64 << (s % 64);
            }
            dd = dd.shr(1);
        }
        q
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.to_be_bytes())
    }

    pub fn from_hex(s: &str) -> Option<U256> {
        let v = hex::decode(s).ok()?;
        let b: [u8; 32] = v.try_into().ok()?;
        Some(U256::from_be_bytes(&b))
    }

    /// Lossy log2 for display.
    pub fn log2_f64(&self) -> f64 {
        let b = self.bits();
        if b == 0 {
            return f64::NEG_INFINITY;
        }
        let top = self.shr(b.saturating_sub(53));
        (top.0[0] as f64).log2() + b.saturating_sub(53) as f64
    }
}

impl PartialOrd for U256 {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for U256 {
    fn cmp(&self, o: &Self) -> Ordering {
        for i in (0..4).rev() {
            match self.0[i].cmp(&o.0[i]) {
                Ordering::Equal => continue,
                x => return x,
            }
        }
        Ordering::Equal
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip_and_ops() {
        let mut b = [0u8; 32];
        b[0] = 0x00;
        b[1] = 0xff;
        b[31] = 7;
        let x = U256::from_be_bytes(&b);
        assert_eq!(x.to_be_bytes(), b);
        assert_eq!(x.bits(), 248);
        let y = x.saturating_mul_u64(3).div_u64(3);
        assert_eq!(x, y);
        assert_eq!(U256::from_u64(100).div_u256(&U256::from_u64(7)), U256::from_u64(14));
        assert_eq!(U256::work_for_target(&U256::MAX.shr(8)), U256::from_u64(256));
        assert_eq!(x.shl(4).shr(4), x);
    }
}
