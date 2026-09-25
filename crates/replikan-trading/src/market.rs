//! Public top-of-book collection, separate from agent price assertions.
//! Local request/receipt time bounds freshness; Binance bookTicker has no
//! exchange event timestamp. This is not a full-depth execution simulator.

use crate::financial::{Price, Quantity, ReferencePrice};
use crate::orders::Side;
use crate::{Config, Error, Result};
use replikan_market_http::{HttpPolicy, HttpTransport, ReqwestHttpTransport};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const PROVIDER: &str = "binance_spot_public";
pub const MAX_RESPONSE_BYTES: usize = 16_384;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketDataPolicy {
    pub instrument_id: String,
    pub symbol: String,
    pub max_age_ms: i64,
    pub min_refresh_interval_ms: i64,
}

impl MarketDataPolicy {
    pub(crate) fn validate(&self, config: &Config) -> Result<()> {
        let rules = config
            .instruments
            .get(&self.instrument_id)
            .ok_or_else(|| Error("market instrument is not configured".into()))?;
        if config.instruments.len() != 1
            || self.symbol.is_empty()
            || self.symbol.len() > 30
            || !self
                .symbol
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
            || self.symbol != format!("{}{}", rules.base_asset, rules.quote_asset)
            || !(1000..=60_000).contains(&self.max_age_ms)
            || !(1000..=60_000).contains(&self.min_refresh_interval_ms)
        {
            return Err(Error(
                "invalid public market identity or freshness policy".into(),
            ));
        }
        Ok(())
    }

    pub fn endpoint(&self) -> String {
        format!(
            "https://data-api.binance.vision/api/v3/ticker/bookTicker?symbol={}&symbolStatus=TRADING",
            self.symbol
        )
    }

    pub fn transport(&self) -> Result<ReqwestHttpTransport> {
        let policy = HttpPolicy::new(
            vec!["data-api.binance.vision".into()],
            MAX_RESPONSE_BYTES,
            10_000,
            20_000,
        )
        .map_err(|e| Error(e.to_string()))?;
        if let Some(path) = std::env::var_os("SSL_CERT_FILE") {
            use std::io::Read;
            let file = std::fs::File::open(path)
                .map_err(|_| Error("operator CA bundle cannot be opened".into()))?;
            let mut bytes = Vec::new();
            file.take(1_048_577)
                .read_to_end(&mut bytes)
                .map_err(|_| Error("operator CA bundle cannot be read".into()))?;
            ReqwestHttpTransport::new_with_ca_bundle(policy, &bytes)
                .map_err(|e| Error(e.to_string()))
        } else {
            ReqwestHttpTransport::new(policy).map_err(|e| Error(e.to_string()))
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BookTicker {
    symbol: String,
    bid_price: Price,
    bid_qty: Quantity,
    ask_price: Price,
    ask_qty: Quantity,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketSnapshot {
    snapshot_id: String,
    pub provider: String,
    pub instrument_id: String,
    pub symbol: String,
    pub request_started_at_ms: i64,
    pub received_at_ms: i64,
    pub bid: Price,
    pub bid_quantity: Quantity,
    pub ask: Price,
    pub ask_quantity: Quantity,
    pub raw_response: String,
    pub response_sha256: String,
}

impl MarketSnapshot {
    pub fn id(&self) -> &str {
        &self.snapshot_id
    }

    fn from_response(
        policy: &MarketDataPolicy,
        body: String,
        start: i64,
        received: i64,
    ) -> Result<Self> {
        if body.len() > MAX_RESPONSE_BYTES
            || start < 0
            || received < start
            || received
                .checked_sub(start)
                .is_none_or(|age| age >= policy.max_age_ms)
        {
            return Err(Error(
                "public quote exceeded size, latency or clock bounds".into(),
            ));
        }
        let book: BookTicker = serde_json::from_str(&body)?;
        if book.symbol != policy.symbol || book.bid_price > book.ask_price {
            return Err(Error("public quote symbol mismatch or crossed book".into()));
        }
        let response_sha256 = format!("{:x}", Sha256::digest(body.as_bytes()));
        let identity = serde_json::to_vec(&(
            PROVIDER,
            &policy.instrument_id,
            &policy.symbol,
            start,
            received,
            &response_sha256,
        ))?;
        let snapshot_id = format!("{:x}", Sha256::digest(identity));
        Ok(Self {
            snapshot_id,
            provider: PROVIDER.into(),
            instrument_id: policy.instrument_id.clone(),
            symbol: policy.symbol.clone(),
            request_started_at_ms: start,
            received_at_ms: received,
            bid: book.bid_price,
            bid_quantity: book.bid_qty,
            ask: book.ask_price,
            ask_quantity: book.ask_qty,
            raw_response: body,
            response_sha256,
        })
    }

    pub(crate) fn validate(&self, policy: &MarketDataPolicy) -> Result<()> {
        let reconstructed = Self::from_response(
            policy,
            self.raw_response.clone(),
            self.request_started_at_ms,
            self.received_at_ms,
        )?;
        if &reconstructed != self {
            return Err(Error("market snapshot identity/content mismatch".into()));
        }
        Ok(())
    }

    pub fn reference(&self, policy: &MarketDataPolicy, side: Side) -> Result<ReferencePrice> {
        Ok(ReferencePrice {
            venue: "paper".into(),
            instrument_id: self.instrument_id.clone(),
            price: if side == Side::Buy {
                self.ask
            } else {
                self.bid
            },
            observed_at_ms: self.request_started_at_ms,
            valid_until_ms: self
                .request_started_at_ms
                .checked_add(policy.max_age_ms)
                .ok_or_else(|| Error("quote validity overflow".into()))?,
        })
    }

    pub fn validate_at(&self, policy: &MarketDataPolicy, now_ms: i64) -> Result<()> {
        if now_ms < self.received_at_ms {
            return Err(Error("quote receipt is in the future".into()));
        }
        self.reference(policy, Side::Buy)?.validate_at(now_ms)?;
        Ok(())
    }

    pub(crate) fn capacity(&self, side: Side) -> Quantity {
        if side == Side::Buy {
            self.ask_quantity
        } else {
            self.bid_quantity
        }
    }
}

/// Transport and clock are trusted host capabilities, not agent tool arguments.
pub fn collect<T: HttpTransport, F: FnMut() -> Result<i64>>(
    policy: &MarketDataPolicy,
    transport: &T,
    mut clock: F,
) -> Result<MarketSnapshot> {
    let started = clock()?;
    let response = transport
        .get(&policy.endpoint())
        .map_err(|e| Error(e.to_string()))?;
    let received = clock()?;
    if response.status != 200 {
        return Err(Error(format!(
            "public feed HTTP {}: no automatic retry",
            response.status
        )));
    }
    MarketSnapshot::from_response(policy, response.body, started, received)
}

pub fn now_ms() -> Result<i64> {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| Error("system clock precedes epoch".into()))?;
    i64::try_from(elapsed.as_millis()).map_err(|_| Error("timestamp overflow".into()))
}
