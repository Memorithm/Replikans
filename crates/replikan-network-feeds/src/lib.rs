#![forbid(unsafe_code)]

use core::fmt;
use replikan_core::BasisPoints;
use replikan_market_http::{HttpPolicy, HttpResponse, HttpTransport, TransportError};
use replikan_mining_market::network_consensus::{NetworkConsensusError, NetworkObservation};
use replikan_opportunities::EvidenceRef;
use serde_json::Value;

const DEFAULT_MAX_RESPONSE_BYTES: usize = 256 * 1024;
const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 3_000;
const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 8_000;
const BITCOIN_HALVING_INTERVAL: u64 = 210_000;
const BITCOIN_TARGET_BLOCK_SECONDS: u64 = 600;
const BITCOIN_INITIAL_SUBSIDY_SATS: u64 = 5_000_000_000;
const H_PER_TH_DECIMAL_EXPONENT: u32 = 12;
const MAX_U128_DECIMAL_EXPONENT: u32 = 38;

const MEMPOOL_HASHRATE_ENDPOINT: &str = "https://mempool.space/api/v1/mining/hashrate/3d";
const MEMPOOL_HEIGHT_ENDPOINT: &str = "https://mempool.space/api/blocks/tip/height";
const BLOCKCHAIN_HASHRATE_ENDPOINT: &str = "https://api.blockchain.info/charts/hash-rate?timespan=3days&rollingAverage=24hours&format=json&sampled=false";
const BLOCKCHAIN_HEIGHT_ENDPOINT: &str = "https://blockchain.info/q/getblockcount";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BitcoinNetworkFeed {
    MempoolSpace,
    BlockchainCom,
}

impl BitcoinNetworkFeed {
    #[must_use]
    pub const fn source_id(self) -> &'static str {
        match self {
            Self::MempoolSpace => "mempool.space",
            Self::BlockchainCom => "blockchain.com",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BitcoinNetworkRequest {
    pub feed: BitcoinNetworkFeed,
    pub horizon_seconds: u64,
    pub ttl_ms: u64,
    pub confidence: BasisPoints,
}

pub fn bitcoin_public_http_policy() -> Result<HttpPolicy, TransportError> {
    HttpPolicy::new(
        vec![
            "mempool.space".to_owned(),
            "api.blockchain.info".to_owned(),
            "blockchain.info".to_owned(),
        ],
        DEFAULT_MAX_RESPONSE_BYTES,
        DEFAULT_CONNECT_TIMEOUT_MS,
        DEFAULT_REQUEST_TIMEOUT_MS,
    )
}

pub fn fetch_bitcoin_network_observation<T>(
    transport: &T,
    request: &BitcoinNetworkRequest,
    observed_at_unix_ms: u64,
) -> Result<NetworkObservation, NetworkFeedError>
where
    T: HttpTransport,
{
    if request.horizon_seconds == 0 {
        return Err(NetworkFeedError::ZeroHorizon);
    }
    if request.ttl_ms == 0 {
        return Err(NetworkFeedError::ZeroTtl);
    }
    let valid_until_unix_ms = observed_at_unix_ms
        .checked_add(request.ttl_ms)
        .ok_or(NetworkFeedError::TimestampOverflow)?;

    let (hashrate, height, evidence) = match request.feed {
        BitcoinNetworkFeed::MempoolSpace => {
            let hashrate_body = get_success_body(transport, MEMPOOL_HASHRATE_ENDPOINT)?;
            let height_body = get_success_body(transport, MEMPOOL_HEIGHT_ENDPOINT)?;
            (
                parse_mempool_hashrate(&hashrate_body)?,
                parse_block_height(&height_body)?,
                evidence_pair(
                    "network:mempool.space:hashrate:3d",
                    "network:mempool.space:tip-height",
                )?,
            )
        }
        BitcoinNetworkFeed::BlockchainCom => {
            let hashrate_body = get_success_body(transport, BLOCKCHAIN_HASHRATE_ENDPOINT)?;
            let height_body = get_success_body(transport, BLOCKCHAIN_HEIGHT_ENDPOINT)?;
            (
                parse_blockchain_hashrate(&hashrate_body)?,
                parse_block_height(&height_body)?,
                evidence_pair(
                    "network:blockchain.com:hash-rate:3d-24h-average",
                    "network:blockchain.com:tip-height",
                )?,
            )
        }
    };

    let network_emission_atoms =
        expected_bitcoin_subsidy_emission_sats(height, request.horizon_seconds)?;

    NetworkObservation::new(
        request.feed.source_id(),
        "BTC",
        "sha256d",
        hashrate,
        network_emission_atoms,
        request.horizon_seconds,
        observed_at_unix_ms,
        valid_until_unix_ms,
        request.confidence,
        evidence,
    )
    .map_err(NetworkFeedError::NetworkObservation)
}

fn get_success_body<T>(transport: &T, endpoint: &str) -> Result<String, NetworkFeedError>
where
    T: HttpTransport,
{
    let HttpResponse { status, body } = transport
        .get(endpoint)
        .map_err(NetworkFeedError::Transport)?;
    if !(200..=299).contains(&status) {
        return Err(NetworkFeedError::HttpStatus(status));
    }
    Ok(body)
}

fn parse_mempool_hashrate(body: &str) -> Result<u128, NetworkFeedError> {
    let value: Value = serde_json::from_str(body).map_err(NetworkFeedError::Json)?;
    let rate = value
        .get("currentHashrate")
        .or_else(|| {
            value
                .get("hashrates")
                .and_then(Value::as_array)
                .and_then(|items| items.last())
                .and_then(|item| item.get("avgHashrate"))
        })
        .ok_or(NetworkFeedError::MissingField(
            "currentHashrate/hashrates[-1].avgHashrate",
        ))?;
    parse_json_decimal_scaled_floor(rate, 0)
}

fn parse_blockchain_hashrate(body: &str) -> Result<u128, NetworkFeedError> {
    let value: Value = serde_json::from_str(body).map_err(NetworkFeedError::Json)?;
    let unit = value
        .get("unit")
        .and_then(Value::as_str)
        .ok_or(NetworkFeedError::MissingField("unit"))?;
    if unit != "TH/s" {
        return Err(NetworkFeedError::UnexpectedUnit(unit.to_owned()));
    }
    let rate_th = value
        .get("values")
        .and_then(Value::as_array)
        .and_then(|items| items.last())
        .and_then(|item| item.get("y"))
        .ok_or(NetworkFeedError::MissingField("values[-1].y"))?;
    parse_json_decimal_scaled_floor(rate_th, H_PER_TH_DECIMAL_EXPONENT)
}

fn parse_block_height(body: &str) -> Result<u64, NetworkFeedError> {
    let value = body.trim();
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(NetworkFeedError::InvalidBlockHeight);
    }
    value
        .parse::<u64>()
        .map_err(|_| NetworkFeedError::InvalidBlockHeight)
}

fn parse_json_decimal_scaled_floor(
    value: &Value,
    scale_decimal_exponent: u32,
) -> Result<u128, NetworkFeedError> {
    let raw = match value {
        Value::Number(number) => number.to_string(),
        Value::String(value) => value.clone(),
        _ => return Err(NetworkFeedError::InvalidDecimal),
    };
    parse_decimal_scaled_floor(&raw, scale_decimal_exponent)
}

fn parse_decimal_scaled_floor(
    value: &str,
    scale_decimal_exponent: u32,
) -> Result<u128, NetworkFeedError> {
    let value = value.trim();
    if value.is_empty() || value.starts_with('-') {
        return Err(NetworkFeedError::InvalidDecimal);
    }

    let (mantissa, exponent) = split_exponent(value)?;
    let mut decimal_parts = mantissa.split('.');
    let whole = decimal_parts
        .next()
        .ok_or(NetworkFeedError::InvalidDecimal)?;
    let fraction = decimal_parts.next().unwrap_or("");
    if decimal_parts.next().is_some()
        || whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(NetworkFeedError::InvalidDecimal);
    }

    let mut digits = String::with_capacity(whole.len() + fraction.len());
    digits.push_str(whole);
    digits.push_str(fraction);
    let significant = digits
        .parse::<u128>()
        .map_err(|_| NetworkFeedError::NumericOverflow)?;
    if significant == 0 {
        return Ok(0);
    }

    let fraction_digits =
        i32::try_from(fraction.len()).map_err(|_| NetworkFeedError::NumericOverflow)?;
    let scale =
        i32::try_from(scale_decimal_exponent).map_err(|_| NetworkFeedError::NumericOverflow)?;
    let net_exponent = exponent
        .checked_sub(fraction_digits)
        .and_then(|value| value.checked_add(scale))
        .ok_or(NetworkFeedError::NumericOverflow)?;

    if net_exponent >= 0 {
        let exponent =
            u32::try_from(net_exponent).map_err(|_| NetworkFeedError::NumericOverflow)?;
        let multiplier = pow10(exponent)?;
        significant
            .checked_mul(multiplier)
            .ok_or(NetworkFeedError::NumericOverflow)
    } else {
        let exponent = net_exponent.unsigned_abs();
        if exponent > MAX_U128_DECIMAL_EXPONENT {
            return Ok(0);
        }
        Ok(significant / pow10(exponent)?)
    }
}

fn split_exponent(value: &str) -> Result<(&str, i32), NetworkFeedError> {
    let mut index = None;
    for (position, byte) in value.bytes().enumerate() {
        if matches!(byte, b'e' | b'E') {
            if index.is_some() {
                return Err(NetworkFeedError::InvalidDecimal);
            }
            index = Some(position);
        }
    }

    match index {
        Some(position) => {
            let mantissa = &value[..position];
            let exponent = &value[position + 1..];
            if exponent.is_empty() {
                return Err(NetworkFeedError::InvalidDecimal);
            }
            let exponent = exponent
                .parse::<i32>()
                .map_err(|_| NetworkFeedError::InvalidDecimal)?;
            Ok((mantissa, exponent))
        }
        None => Ok((value, 0)),
    }
}

fn pow10(exponent: u32) -> Result<u128, NetworkFeedError> {
    if exponent > MAX_U128_DECIMAL_EXPONENT {
        return Err(NetworkFeedError::NumericOverflow);
    }
    let mut value = 1_u128;
    for _ in 0..exponent {
        value = value
            .checked_mul(10)
            .ok_or(NetworkFeedError::NumericOverflow)?;
    }
    Ok(value)
}

#[must_use]
pub const fn bitcoin_subsidy_sats(block_height: u64) -> u64 {
    let halvings = block_height / BITCOIN_HALVING_INTERVAL;
    if halvings >= 64 {
        0
    } else {
        BITCOIN_INITIAL_SUBSIDY_SATS >> halvings
    }
}

pub fn expected_bitcoin_subsidy_emission_sats(
    tip_height: u64,
    horizon_seconds: u64,
) -> Result<u128, NetworkFeedError> {
    let mut remaining_blocks = horizon_seconds / BITCOIN_TARGET_BLOCK_SECONDS;
    if remaining_blocks == 0 {
        return Ok(0);
    }

    let mut next_height = tip_height
        .checked_add(1)
        .ok_or(NetworkFeedError::NumericOverflow)?;
    let mut emission = 0_u128;

    while remaining_blocks > 0 {
        let subsidy = bitcoin_subsidy_sats(next_height);
        if subsidy == 0 {
            break;
        }
        let halvings = next_height / BITCOIN_HALVING_INTERVAL;
        let next_boundary = halvings
            .checked_add(1)
            .and_then(|value| value.checked_mul(BITCOIN_HALVING_INTERVAL))
            .ok_or(NetworkFeedError::NumericOverflow)?;
        let blocks_until_boundary = next_boundary
            .checked_sub(next_height)
            .ok_or(NetworkFeedError::NumericOverflow)?;
        let blocks_in_epoch = remaining_blocks.min(blocks_until_boundary);
        let epoch_emission = u128::from(subsidy)
            .checked_mul(u128::from(blocks_in_epoch))
            .ok_or(NetworkFeedError::NumericOverflow)?;
        emission = emission
            .checked_add(epoch_emission)
            .ok_or(NetworkFeedError::NumericOverflow)?;
        remaining_blocks -= blocks_in_epoch;
        next_height = next_height
            .checked_add(blocks_in_epoch)
            .ok_or(NetworkFeedError::NumericOverflow)?;
    }

    Ok(emission)
}

fn evidence_pair(first: &str, second: &str) -> Result<Vec<EvidenceRef>, NetworkFeedError> {
    let first = EvidenceRef::new(first).map_err(|_| NetworkFeedError::InvalidEvidence)?;
    let second = EvidenceRef::new(second).map_err(|_| NetworkFeedError::InvalidEvidence)?;
    Ok(vec![first, second])
}

#[derive(Debug)]
pub enum NetworkFeedError {
    ZeroHorizon,
    ZeroTtl,
    TimestampOverflow,
    Transport(TransportError),
    HttpStatus(u16),
    Json(serde_json::Error),
    MissingField(&'static str),
    UnexpectedUnit(String),
    InvalidDecimal,
    NumericOverflow,
    InvalidBlockHeight,
    InvalidEvidence,
    NetworkObservation(NetworkConsensusError),
}

impl fmt::Display for NetworkFeedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroHorizon => write!(f, "network observation horizon must be greater than zero"),
            Self::ZeroTtl => write!(f, "network observation TTL must be greater than zero"),
            Self::TimestampOverflow => write!(f, "network observation validity timestamp overflow"),
            Self::Transport(error) => write!(f, "network transport failed: {error}"),
            Self::HttpStatus(status) => write!(f, "network endpoint returned HTTP status {status}"),
            Self::Json(error) => write!(f, "invalid network provider JSON: {error}"),
            Self::MissingField(field) => write!(f, "network provider response missing {field}"),
            Self::UnexpectedUnit(unit) => write!(f, "unexpected network metric unit: {unit}"),
            Self::InvalidDecimal => write!(f, "invalid decimal network metric"),
            Self::NumericOverflow => write!(f, "network metric exceeds integer range"),
            Self::InvalidBlockHeight => write!(f, "invalid Bitcoin block height"),
            Self::InvalidEvidence => write!(f, "invalid network evidence reference"),
            Self::NetworkObservation(error) => write!(f, "invalid network observation: {error}"),
        }
    }
}

impl std::error::Error for NetworkFeedError {}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_000_000;
    const EXPECTED_HASHRATE: u128 = 650_000_000_000_000_000_000;

    struct FixtureTransport {
        status: u16,
    }

    impl HttpTransport for FixtureTransport {
        fn get(&self, endpoint: &str) -> Result<HttpResponse, TransportError> {
            let body = if endpoint == MEMPOOL_HASHRATE_ENDPOINT {
                r#"{"hashrates":[{"timestamp":1,"avgHashrate":6.4e20}],"currentHashrate":6.5e20,"currentDifficulty":1}"#.to_owned()
            } else if endpoint == MEMPOOL_HEIGHT_ENDPOINT {
                "840000".to_owned()
            } else if endpoint == BLOCKCHAIN_HASHRATE_ENDPOINT {
                r#"{"status":"ok","unit":"TH/s","values":[{"x":1,"y":6.4e8},{"x":2,"y":6.5e8}]}"#
                    .to_owned()
            } else if endpoint == BLOCKCHAIN_HEIGHT_ENDPOINT {
                "840000".to_owned()
            } else {
                return Err(TransportError::HostForbidden(
                    "unexpected fixture endpoint".to_owned(),
                ));
            };
            Ok(HttpResponse {
                status: self.status,
                body,
            })
        }
    }

    fn bps(value: u32) -> BasisPoints {
        match BasisPoints::new(value) {
            Ok(value) => value,
            Err(error) => unreachable!("valid basis points: {error}"),
        }
    }

    fn request(feed: BitcoinNetworkFeed) -> BitcoinNetworkRequest {
        BitcoinNetworkRequest {
            feed,
            horizon_seconds: 86_400,
            ttl_ms: 60_000,
            confidence: bps(9_000),
        }
    }

    #[test]
    fn fixed_http_policy_allows_only_reviewed_bitcoin_hosts() {
        let policy = match bitcoin_public_http_policy() {
            Ok(value) => value,
            Err(error) => unreachable!("valid bitcoin policy: {error}"),
        };
        assert!(policy.validate_endpoint(MEMPOOL_HASHRATE_ENDPOINT).is_ok());
        assert!(
            policy
                .validate_endpoint(BLOCKCHAIN_HASHRATE_ENDPOINT)
                .is_ok()
        );
        assert!(matches!(
            policy.validate_endpoint("https://example.com/network"),
            Err(TransportError::HostForbidden(_))
        ));
    }

    #[test]
    fn subsidy_schedule_matches_bitcoin_halving_boundaries() {
        assert_eq!(bitcoin_subsidy_sats(0), 5_000_000_000);
        assert_eq!(bitcoin_subsidy_sats(209_999), 5_000_000_000);
        assert_eq!(bitcoin_subsidy_sats(210_000), 2_500_000_000);
        assert_eq!(bitcoin_subsidy_sats(840_000), 312_500_000);
        assert_eq!(bitcoin_subsidy_sats(BITCOIN_HALVING_INTERVAL * 64), 0);
    }

    #[test]
    fn emission_crossing_halving_is_derived_epoch_by_epoch() {
        let emission = expected_bitcoin_subsidy_emission_sats(839_998, 1_200);
        assert_eq!(emission, Ok(937_500_000));
    }

    #[test]
    fn arbitrary_precision_decimal_parser_handles_scientific_notation() {
        assert_eq!(
            parse_decimal_scaled_floor("6.5e8", H_PER_TH_DECIMAL_EXPONENT),
            Ok(EXPECTED_HASHRATE)
        );
        assert_eq!(parse_decimal_scaled_floor("1.999", 0), Ok(1));
        assert_eq!(parse_decimal_scaled_floor("1e-999999", 0), Ok(0));
        assert!(matches!(
            parse_decimal_scaled_floor("1e999999", 0),
            Err(NetworkFeedError::NumericOverflow)
        ));
    }

    #[test]
    fn mempool_feed_produces_conservative_bitcoin_network_observation() {
        let observation = match fetch_bitcoin_network_observation(
            &FixtureTransport { status: 200 },
            &request(BitcoinNetworkFeed::MempoolSpace),
            NOW,
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid mempool observation: {error}"),
        };

        assert_eq!(observation.source_id, "mempool.space");
        assert_eq!(observation.asset_symbol, "BTC");
        assert_eq!(observation.algorithm, "sha256d");
        assert_eq!(observation.network_hashrate_units, EXPECTED_HASHRATE);
        assert_eq!(observation.network_emission_atoms, 45_000_000_000);
        assert_eq!(observation.evidence.len(), 2);
    }

    #[test]
    fn blockchain_feed_normalizes_terahashes_to_hashes_per_second() {
        let observation = match fetch_bitcoin_network_observation(
            &FixtureTransport { status: 200 },
            &request(BitcoinNetworkFeed::BlockchainCom),
            NOW,
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid blockchain.com observation: {error}"),
        };

        assert_eq!(observation.source_id, "blockchain.com");
        assert_eq!(observation.network_hashrate_units, EXPECTED_HASHRATE);
        assert_eq!(observation.network_emission_atoms, 45_000_000_000);
    }

    #[test]
    fn non_success_status_fails_closed() {
        let result = fetch_bitcoin_network_observation(
            &FixtureTransport { status: 503 },
            &request(BitcoinNetworkFeed::MempoolSpace),
            NOW,
        );
        assert!(matches!(result, Err(NetworkFeedError::HttpStatus(503))));
    }
}
