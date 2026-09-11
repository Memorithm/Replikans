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
    pub const fn is_zero(self) -> bool {
        self.micros == 0
    }

    #[must_use]
    pub const fn abs(self) -> Option<Self> {
        match self.micros.checked_abs() {
            Some(value) => Some(Self { micros: value }),
            None => None,
        }
    }

    /// Convert whole quote-currency units into micros without overflow.
    #[must_use]
    pub const fn try_from_units(units: i128) -> Option<Self> {
        match units.checked_mul(1_000_000) {
            Some(micros) => Some(Self { micros }),
            None => None,
        }
    }

    #[must_use]
    pub fn checked_add(self, rhs: Self) -> Option<Self> {
        self.micros.checked_add(rhs.micros).map(Self::from_micros)
    }

    #[must_use]
    pub fn checked_sub(self, rhs: Self) -> Option<Self> {
        self.micros.checked_sub(rhs.micros).map(Self::from_micros)
    }

    #[must_use]
    pub fn checked_mul_i128(self, rhs: i128) -> Option<Self> {
        self.micros.checked_mul(rhs).map(Self::from_micros)
    }

    /// Scale by basis points using truncating integer division (10_000 bps = 100%).
    #[must_use]
    pub fn checked_scale_bps(self, bps: BasisPoints) -> Option<Self> {
        self.micros
            .checked_mul(i128::from(bps.value()))
            .and_then(|scaled| scaled.checked_div(i128::from(BasisPoints::FULL_SCALE)))
            .map(Self::from_micros)
    }
}

impl Add for Money {
    type Output = Self;

    /// Convenience operator for small, already-validated values.
    /// Financial policy paths must use [`Money::checked_add`].
    fn add(self, rhs: Self) -> Self::Output {
        match self.checked_add(rhs) {
            Some(value) => value,
            None => {
                let micros = if self.micros < 0 || rhs.micros < 0 {
                    i128::MIN
                } else {
                    i128::MAX
                };
                Self { micros }
            }
        }
    }
}

impl Sub for Money {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        match self.checked_sub(rhs) {
            Some(value) => value,
            None => {
                let micros = if self.micros >= rhs.micros {
                    i128::MAX
                } else {
                    i128::MIN
                };
                Self { micros }
            }
        }
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
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
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
        assert_eq!(
            BasisPoints::new(10_001),
            Err(RatioError::OutOfRange(10_001))
        );
    }

    #[test]
    fn public_identity_rejects_blank_values() {
        assert_eq!(PublicIdentity::new("   "), Err(IdentityError::Empty));
    }

    #[test]
    fn money_checked_arithmetic_rejects_overflow() {
        let max = Money::from_micros(i128::MAX);
        assert_eq!(max.checked_add(Money::from_micros(1)), None);
        assert_eq!(
            Money::from_micros(i128::MIN).checked_sub(Money::from_micros(1)),
            None
        );
        assert_eq!(Money::from_micros(2).checked_mul_i128(i128::MAX), None);
        assert_eq!(Money::try_from_units(i128::MAX), None);
        assert_eq!(
            Money::try_from_units(2),
            Some(Money::from_micros(2_000_000))
        );
    }

    #[test]
    fn money_scales_by_basis_points_with_truncation() {
        let principal = Money::from_micros(1_000_000);
        let half = match BasisPoints::new(5_000) {
            Ok(value) => value,
            Err(error) => unreachable!("valid basis points: {error}"),
        };
        assert_eq!(
            principal.checked_scale_bps(half),
            Some(Money::from_micros(500_000))
        );
    }

    #[test]
    fn money_add_operator_saturates_instead_of_wrapping() {
        let saturated = Money::from_micros(i128::MAX) + Money::from_micros(1);
        assert_eq!(saturated.micros(), i128::MAX);
        assert!(saturated.is_positive());
    }
}
