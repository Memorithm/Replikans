#![forbid(unsafe_code)]

use core::fmt;
use core::ops::{Add, Sub};

/// Fixed-point monetary value expressed in millionths of the configured quote currency.
///
/// Replikans deliberately avoids binary floating-point arithmetic for treasury and
/// survival decisions. The quote currency itself is selected at runtime by higher layers.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct Money {
    micros: i128,
}

impl Money {
    pub const ZERO: Self = Self { micros: 0 };

    #[must_use]
    pub const fn from_micros(micros: i128) -> Self {
        Self { micros }
    }

    #[must_use]
    pub const fn micros(self) -> i128 {
        self.micros
    }

    #[must_use]
    pub const fn is_positive(self) -> bool {
        self.micros > 0
    }

    #[must_use]
    pub const fn is_negative(self) -> bool {
        self.micros < 0
    }

    #[must_use]
    pub fn checked_add(self, rhs: Self) -> Option<Self> {
        self.micros.checked_add(rhs.micros).map(Self::from_micros)
    }

    #[must_use]
    pub fn checked_sub(self, rhs: Self) -> Option<Self> {
        self.micros.checked_sub(rhs.micros).map(Self::from_micros)
    }
}

impl Add for Money {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self::from_micros(self.micros + rhs.micros)
    }
}

impl Sub for Money {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Self::from_micros(self.micros - rhs.micros)
    }
}

impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.micros < 0 { "-" } else { "" };
        let abs = self.micros.unsigned_abs();
        write!(f, "{sign}{}.{:06}", abs / 1_000_000, abs % 1_000_000)
    }
}

/// Basis-points ratio. 10_000 bps == 100%.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct BasisPoints(u32);

impl BasisPoints {
    pub const FULL_SCALE: u32 = 10_000;

    pub fn new(value: u32) -> Result<Self, RatioError> {
        if value <= Self::FULL_SCALE {
            Ok(Self(value))
        } else {
            Err(RatioError::OutOfRange(value))
        }
    }

    #[must_use]
    pub const fn value(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RatioError {
    OutOfRange(u32),
}

impl fmt::Display for RatioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfRange(value) => write!(f, "basis points out of range: {value}"),
        }
    }
}

impl std::error::Error for RatioError {}

/// Public identifier for a signing identity. It is deliberately not a secret.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicIdentity(String);

impl PublicIdentity {
    pub fn new(value: impl Into<String>) -> Result<Self, IdentityError> {
        let value = value.into();
        if value.trim().is_empty() {
            Err(IdentityError::Empty)
        } else {
            Ok(Self(value))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdentityError {
    Empty,
}

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "public identity cannot be empty")
    }
}

impl std::error::Error for IdentityError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_uses_exact_fixed_point_arithmetic() {
        let revenue = Money::from_micros(2_500_001);
        let cost = Money::from_micros(1_250_000);
        assert_eq!((revenue - cost).micros(), 1_250_001);
    }

    #[test]
    fn basis_points_rejects_values_above_one_hundred_percent() {
        assert_eq!(BasisPoints::new(10_001), Err(RatioError::OutOfRange(10_001)));
    }

    #[test]
    fn public_identity_rejects_blank_values() {
        assert_eq!(PublicIdentity::new("   "), Err(IdentityError::Empty));
    }
}
