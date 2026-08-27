#![forbid(unsafe_code)]

use core::fmt;
use replikan_core::{BasisPoints, Money};
use replikan_mining_market::price_consensus::PriceObservation;
use replikan_opportunities::EvidenceRef;
use serde_json::Value;

const MICROS_PER_UNIT: u128 = 1_000_000;

pub trait PublicPriceAdapter {
    fn provider_id(&self) -> &'static str;
    fn endpoint(&self) -> String;

    fn parse_response(
        &self,
        body: &str,
        observed_at_unix_ms: u64,
        valid_until_unix_ms: u64,
        confidence: BasisPoints,
        evidence: Vec<EvidenceRef>,
    ) -> Result<PriceObservation, FeedError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoinbaseExchangePriceAdapter {
    product_id: String,
    asset_symbol: String,
}

impl CoinbaseExchangePriceAdapter {
    pub fn new(
        product_id: impl Into<String>,
        asset_symbol: impl Into<String>,
    ) -> Result<Self, FeedError> {
        Ok(Self {
            product_id: validated_market_identifier(product_id.into())?,
            asset_symbol: validated_asset_symbol(asset_symbol.into())?,
        })
    }

    #[must_use]
    pub fn product_id(&self) -> &str {
        &self.product_id
    }
}

impl PublicPriceAdapter for CoinbaseExchangePriceAdapter {
    fn provider_id(&self) -> &'static str {
        "coinbase-exchange"
    }

    fn endpoint(&self) -> String {
        format!(
            "https://api.exchange.coinbase.com/products/{}/ticker",
            self.product_id
        )
    }

    fn parse_response(
        &self,
        body: &str,
        observed_at_unix_ms: u64,
        valid_until_unix_ms: u64,
        confidence: BasisPoints,
        evidence: Vec<EvidenceRef>,
    ) -> Result<PriceObservation, FeedError> {
        let value: Value = serde_json::from_str(body).map_err(FeedError::Json)?;
        let price = value
            .get("price")
            .and_then(Value::as_str)
            .ok_or(FeedError::MissingField("price"))?;

        make_observation(
            self.provider_id(),
            &self.asset_symbol,
            price,
            observed_at_unix_ms,
            valid_until_unix_ms,
            confidence,
            evidence,
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KrakenPriceAdapter {
    pair: String,
    asset_symbol: String,
}

impl KrakenPriceAdapter {
    pub fn new(
        pair: impl Into<String>,
        asset_symbol: impl Into<String>,
    ) -> Result<Self, FeedError> {
        Ok(Self {
            pair: validated_market_identifier(pair.into())?,
            asset_symbol: validated_asset_symbol(asset_symbol.into())?,
        })
    }

    #[must_use]
    pub fn pair(&self) -> &str {
        &self.pair
    }
}

impl PublicPriceAdapter for KrakenPriceAdapter {
    fn provider_id(&self) -> &'static str {
        "kraken"
    }

    fn endpoint(&self) -> String {
        format!("https://api.kraken.com/0/public/Ticker?pair={}", self.pair)
    }

    fn parse_response(
        &self,
        body: &str,
        observed_at_unix_ms: u64,
        valid_until_unix_ms: u64,
        confidence: BasisPoints,
        evidence: Vec<EvidenceRef>,
    ) -> Result<PriceObservation, FeedError> {
        let value: Value = serde_json::from_str(body).map_err(FeedError::Json)?;
        let errors = value
            .get("error")
            .and_then(Value::as_array)
            .ok_or(FeedError::MissingField("error"))?;
        if !errors.is_empty() {
            return Err(FeedError::ProviderError);
        }

        let result = value
            .get("result")
            .and_then(Value::as_object)
            .ok_or(FeedError::MissingField("result"))?;
        if result.len() != 1 {
            return Err(FeedError::AmbiguousTickerResult(result.len()));
        }
        let ticker = result
            .values()
            .next()
            .ok_or(FeedError::MissingField("result ticker"))?;
        let price = ticker
            .get("c")
            .and_then(Value::as_array)
            .and_then(|last_trade| last_trade.first())
            .and_then(Value::as_str)
            .ok_or(FeedError::MissingField("result.*.c[0]"))?;

        make_observation(
            self.provider_id(),
            &self.asset_symbol,
            price,
            observed_at_unix_ms,
            valid_until_unix_ms,
            confidence,
            evidence,
        )
    }
}

fn make_observation(
    source_id: &str,
    asset_symbol: &str,
    decimal_price: &str,
    observed_at_unix_ms: u64,
    valid_until_unix_ms: u64,
    confidence: BasisPoints,
    evidence: Vec<EvidenceRef>,
) -> Result<PriceObservation, FeedError> {
    let price_per_unit = parse_nonnegative_decimal_micros(decimal_price)?;
    PriceObservation::new(
        source_id,
        asset_symbol,
        price_per_unit,
        observed_at_unix_ms,
        valid_until_unix_ms,
        confidence,
        evidence,
    )
    .map_err(FeedError::ConsensusInput)
}

pub fn parse_nonnegative_decimal_micros(value: &str) -> Result<Money, FeedError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(FeedError::InvalidDecimal);
    }
    if value.starts_with('-') {
        return Err(FeedError::NegativeDecimal);
    }

    let mut parts = value.split('.');
    let whole = parts.next().ok_or(FeedError::InvalidDecimal)?;
    let fraction = parts.next();
    if parts.next().is_some() {
        return Err(FeedError::InvalidDecimal);
    }
    if whole.is_empty() && fraction.is_none() {
        return Err(FeedError::InvalidDecimal);
    }
    if !whole.is_empty() && !whole.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(FeedError::InvalidDecimal);
    }

    let whole_value = if whole.is_empty() {
        0
    } else {
        whole
            .parse::<u128>()
            .map_err(|_| FeedError::DecimalOverflow)?
    };
    let mut micros = whole_value
        .checked_mul(MICROS_PER_UNIT)
        .ok_or(FeedError::DecimalOverflow)?;

    if let Some(fraction) = fraction {
        if fraction.is_empty() || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(FeedError::InvalidDecimal);
        }
        let significant = fraction.as_bytes().iter().take(6);
        let mut fraction_micros = 0_u128;
        let mut digits = 0_u32;
        for byte in significant {
            fraction_micros = fraction_micros
                .checked_mul(10)
                .and_then(|current| current.checked_add(u128::from(*byte - b'0')))
                .ok_or(FeedError::DecimalOverflow)?;
            digits += 1;
        }
        for _ in digits..6 {
            fraction_micros = fraction_micros
                .checked_mul(10)
                .ok_or(FeedError::DecimalOverflow)?;
        }
        micros = micros
            .checked_add(fraction_micros)
            .ok_or(FeedError::DecimalOverflow)?;
    }

    let micros = i128::try_from(micros).map_err(|_| FeedError::DecimalOverflow)?;
    Ok(Money::from_micros(micros))
}

fn validated_market_identifier(value: String) -> Result<String, FeedError> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(FeedError::InvalidMarketIdentifier);
    }
    Ok(value)
}

fn validated_asset_symbol(value: String) -> Result<String, FeedError> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(FeedError::InvalidAssetSymbol);
    }
    Ok(value)
}

#[derive(Debug)]
pub enum FeedError {
    InvalidMarketIdentifier,
    InvalidAssetSymbol,
    InvalidDecimal,
    NegativeDecimal,
    DecimalOverflow,
    MissingField(&'static str),
    ProviderError,
    AmbiguousTickerResult(usize),
    Json(serde_json::Error),
    ConsensusInput(replikan_mining_market::price_consensus::PriceConsensusError),
}

impl fmt::Display for FeedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMarketIdentifier => write!(f, "invalid provider market identifier"),
            Self::InvalidAssetSymbol => write!(f, "invalid asset symbol"),
            Self::InvalidDecimal => write!(f, "invalid decimal price"),
            Self::NegativeDecimal => write!(f, "price cannot be negative"),
            Self::DecimalOverflow => write!(f, "decimal price exceeds fixed-point range"),
            Self::MissingField(field) => write!(f, "provider response missing {field}"),
            Self::ProviderError => write!(f, "provider returned an error response"),
            Self::AmbiguousTickerResult(count) => {
                write!(f, "provider returned {count} ticker results instead of one")
            }
            Self::Json(error) => write!(f, "invalid provider JSON: {error}"),
            Self::ConsensusInput(error) => write!(f, "invalid price observation: {error}"),
        }
    }
}

impl std::error::Error for FeedError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn bps(value: u32) -> BasisPoints {
        match BasisPoints::new(value) {
            Ok(value) => value,
            Err(error) => unreachable!("valid basis points: {error}"),
        }
    }

    fn evidence(value: &str) -> EvidenceRef {
        match EvidenceRef::new(value) {
            Ok(value) => value,
            Err(error) => unreachable!("valid evidence: {error}"),
        }
    }

    #[test]
    fn decimal_parser_never_uses_binary_float_and_floors_sub_micro_digits() {
        assert_eq!(
            parse_nonnegative_decimal_micros("6268.48123499").map(Money::micros),
            Ok(6_268_481_234)
        );
        assert_eq!(
            parse_nonnegative_decimal_micros("0.1").map(Money::micros),
            Ok(100_000)
        );
    }

    #[test]
    fn parses_coinbase_exchange_ticker_fixture() {
        let adapter = match CoinbaseExchangePriceAdapter::new("BTC-USD", "BTC") {
            Ok(value) => value,
            Err(error) => unreachable!("valid adapter: {error}"),
        };
        let observation = match adapter.parse_response(
            r#"{"trade_id":86326522,"price":"6268.48","size":"0.006","time":"2020-03-20T00:22:57Z","bid":"6265.15","ask":"6267.71","volume":"53602"}"#,
            1_000_000,
            1_060_000,
            bps(9_000),
            vec![evidence("coinbase:ticker:test")],
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid fixture: {error}"),
        };

        assert_eq!(adapter.provider_id(), "coinbase-exchange");
        assert_eq!(
            observation.price_per_unit,
            Money::from_micros(6_268_480_000)
        );
        assert_eq!(observation.source_id, "coinbase-exchange");
    }

    #[test]
    fn parses_kraken_ticker_fixture() {
        let adapter = match KrakenPriceAdapter::new("XBTUSD", "BTC") {
            Ok(value) => value,
            Err(error) => unreachable!("valid adapter: {error}"),
        };
        let observation = match adapter.parse_response(
            r#"{"error":[],"result":{"XXBTZUSD":{"a":["8466.90000","1","1.000"],"b":["8464.10000","1","1.000"],"c":["8464.50000","0.212"]}}}"#,
            1_000_000,
            1_060_000,
            bps(9_000),
            vec![evidence("kraken:ticker:test")],
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid fixture: {error}"),
        };

        assert_eq!(adapter.provider_id(), "kraken");
        assert_eq!(
            observation.price_per_unit,
            Money::from_micros(8_464_500_000)
        );
        assert_eq!(observation.source_id, "kraken");
    }

    #[test]
    fn rejects_kraken_provider_error_and_ambiguous_results() {
        let adapter = match KrakenPriceAdapter::new("XBTUSD", "BTC") {
            Ok(value) => value,
            Err(error) => unreachable!("valid adapter: {error}"),
        };
        let provider_error = adapter.parse_response(
            r#"{"error":["EQuery:Unknown asset pair"],"result":{}}"#,
            1,
            2,
            bps(9_000),
            vec![evidence("kraken:error")],
        );
        assert!(matches!(provider_error, Err(FeedError::ProviderError)));

        let ambiguous = adapter.parse_response(
            r#"{"error":[],"result":{"A":{"c":["1","1"]},"B":{"c":["2","1"]}}}"#,
            1,
            2,
            bps(9_000),
            vec![evidence("kraken:ambiguous")],
        );
        assert!(matches!(
            ambiguous,
            Err(FeedError::AmbiguousTickerResult(2))
        ));
    }

    #[test]
    fn endpoints_match_documented_public_market_routes() {
        let coinbase = match CoinbaseExchangePriceAdapter::new("BTC-USD", "BTC") {
            Ok(value) => value,
            Err(error) => unreachable!("valid adapter: {error}"),
        };
        let kraken = match KrakenPriceAdapter::new("XBTUSD", "BTC") {
            Ok(value) => value,
            Err(error) => unreachable!("valid adapter: {error}"),
        };

        assert_eq!(
            coinbase.endpoint(),
            "https://api.exchange.coinbase.com/products/BTC-USD/ticker"
        );
        assert_eq!(
            kraken.endpoint(),
            "https://api.kraken.com/0/public/Ticker?pair=XBTUSD"
        );
    }
}
