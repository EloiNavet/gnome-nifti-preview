//! Exact scalar values, kept separate from the float32 grayscale pipeline.

use serde_json::{json, Value};

#[derive(Clone, Copy, Debug)]
pub enum VoxelValue {
    Integer(i128),
    Float(f64),
}

impl VoxelValue {
    pub fn as_f64(self) -> f64 {
        match self {
            Self::Integer(v) => v as f64,
            Self::Float(v) => v,
        }
    }

    pub fn is_finite(self) -> bool {
        match self {
            Self::Integer(_) => true,
            Self::Float(v) => v.is_finite(),
        }
    }

    pub fn is_zero(self) -> bool {
        self == Self::Integer(0)
    }

    pub fn is_negative(self) -> bool {
        match self {
            Self::Integer(v) => v < 0,
            Self::Float(v) => v < 0.0,
        }
    }

    pub fn is_integral(self) -> bool {
        match self {
            Self::Integer(_) => true,
            Self::Float(v) => (v - v.round_ties_even()).abs() <= 1e-3,
        }
    }

    /// Hash/equality key. Normalize integral floats and signed zero, without
    /// rounding an integer through f64 (including i64::MIN and u64::MAX).
    pub fn key(self) -> (u8, u128) {
        match self {
            Self::Integer(v) => (0, v as u128),
            Self::Float(v) if v.is_finite() && v.fract() == 0.0 && v >= i128::MIN as f64 && v < i128::MAX as f64 => {
                (0, (v as i128) as u128)
            }
            Self::Float(v) => (1, v.to_bits() as u128),
        }
    }

    pub fn less_than(self, other: Self) -> bool {
        match (self, other) {
            (Self::Integer(a), Self::Integer(b)) => a < b,
            _ => self.as_f64() < other.as_f64(),
        }
    }

    /// JavaScript numbers cannot carry every 64-bit integer. Such values use
    /// decimal strings on the wire, also in the metadata range.
    pub fn to_json(self) -> Value {
        const SAFE: i128 = (1 << 53) - 1;
        match self {
            Self::Integer(v) if (-SAFE..=SAFE).contains(&v) => json!(v as i64),
            Self::Integer(v) => json!(v.to_string()),
            Self::Float(v) => json!(v),
        }
    }

    pub fn to_label_json(self) -> Value {
        match self {
            Self::Float(v) if v.is_finite() && v.fract() == 0.0 && v.abs() > ((1u64 << 53) - 1) as f64 => {
                json!(format!("{v:.0}"))
            }
            _ => self.to_json(),
        }
    }

    pub fn label_id(self) -> i128 {
        match self {
            Self::Integer(v) => v,
            Self::Float(v) if v.is_finite() => v.round_ties_even() as i128,
            _ => 0,
        }
    }
}

impl PartialEq for VoxelValue {
    fn eq(&self, other: &Self) -> bool {
        self.key() == other.key()
    }
}

impl From<f64> for VoxelValue {
    fn from(v: f64) -> Self {
        Self::Float(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_wire_values_and_compact_intensities() {
        for v in [-(1i128 << 53) - 1, i64::MIN as i128, u64::MAX as i128] {
            assert_eq!(VoxelValue::Integer(v).to_json(), v.to_string());
        }
        assert_eq!(VoxelValue::Integer((1 << 53) - 1).to_json(), json!((1u64 << 53) - 1));
        assert_eq!(VoxelValue::Integer(0), VoxelValue::Float(-0.0));
        assert_ne!(VoxelValue::Integer(1 << 53), VoxelValue::Integer((1 << 53) + 1));
        assert_eq!(VoxelValue::Float(3e38).to_json().as_f64(), Some(3e38));
        assert_eq!(VoxelValue::Float(9007199254740994.0).to_label_json(), "9007199254740994");
    }
}
