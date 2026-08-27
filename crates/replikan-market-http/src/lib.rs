#![forbid(unsafe_code)]

use replikan_core::BasisPoints;
use replikan_market_feeds::{FeedError, PublicPriceAdapter};
use replikan_mining_market::price_consensus::PriceObservation;
use replikan_opportunities::EvidenceRef;
use reqwest::Url;
use reqwest::blocking::Client;
use reqwest::redirect::Policy;
use std::fmt;
use std::io::Read;
use std::time::Duration;

const DEFAULT_MAX_RESPONSE_BYTES: usize = 256 * 1024;
const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 3_000;
const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 8_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpPolicy {
    allowed_hosts: Vec<String>,
    max_response_bytes: usize,
    connect_timeout_ms: u64,
    request_timeout_ms: u64,
}

impl HttpPolicy {
    pub fn new(
        allowed_hosts: Vec<String>,
        max_response_bytes: usize,
        connect_timeout_ms: u64,
        request_timeout_ms: u64,
    ) -> Result<Self, TransportError> {
        if allowed_hosts.is_empty() {
            return Err(TransportError::InvalidPolicy(
                "at least one HTTPS host must be allowed",
            ));
        }
        if max_response_bytes == 0 {
            return Err(TransportError::InvalidPolicy(
                "maximum response size must be greater than zero",
            ));
        }
        if connect_timeout_ms == 0 || request_timeout_ms == 0 {
            return Err(TransportError::InvalidPolicy(
                "timeouts must be greater than zero",
            ));
        }
        if connect_timeout_ms > request_timeout_ms {
            return Err(TransportError::InvalidPolicy(
                "connect timeout cannot exceed request timeout",
            ));
        }

        let mut normalized_hosts = Vec::with_capacity(allowed_hosts.len());
        for host in allowed_hosts {
            normalized_hosts.push(normalize_host(&host)?);
        }
        normalized_hosts.sort();
        normalized_hosts.dedup();

        Ok(Self {
            allowed_hosts: normalized_hosts,
            max_response_bytes,
            connect_timeout_ms,
            request_timeout_ms,
        })
    }

    pub fn public_exchange_defaults() -> Result<Self, TransportError> {
        Self::new(
            vec![
                "api.exchange.coinbase.com".to_owned(),
                "api.kraken.com".to_owned(),
            ],
            DEFAULT_MAX_RESPONSE_BYTES,
            DEFAULT_CONNECT_TIMEOUT_MS,
            DEFAULT_REQUEST_TIMEOUT_MS,
        )
    }

    #[must_use]
    pub fn allowed_hosts(&self) -> &[String] {
        &self.allowed_hosts
    }

    #[must_use]
    pub const fn max_response_bytes(&self) -> usize {
        self.max_response_bytes
    }

    pub fn validate_endpoint(&self, endpoint: &str) -> Result<(), TransportError> {
        self.parse_endpoint(endpoint).map(|_| ())
    }

    fn parse_endpoint(&self, endpoint: &str) -> Result<Url, TransportError> {
        let url = Url::parse(endpoint).map_err(|_| TransportError::InvalidUrl)?;

        if url.scheme() != "https" {
            return Err(TransportError::HttpsRequired);
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(TransportError::CredentialsForbidden);
        }
        if url.fragment().is_some() {
            return Err(TransportError::FragmentForbidden);
        }
        if url.port_or_known_default() != Some(443) {
            return Err(TransportError::NonDefaultPortForbidden);
        }

        let host = url.host_str().ok_or(TransportError::MissingHost)?;
        if !self.allowed_hosts.iter().any(|allowed| allowed == host) {
            return Err(TransportError::HostForbidden(host.to_owned()));
        }

        Ok(url)
    }

    fn connect_timeout(&self) -> Duration {
        Duration::from_millis(self.connect_timeout_ms)
    }

    fn request_timeout(&self) -> Duration {
        Duration::from_millis(self.request_timeout_ms)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

pub trait HttpTransport {
    fn get(&self, endpoint: &str) -> Result<HttpResponse, TransportError>;
}

#[derive(Debug)]
pub struct ReqwestHttpTransport {
    client: Client,
    policy: HttpPolicy,
}

impl ReqwestHttpTransport {
    pub fn new(policy: HttpPolicy) -> Result<Self, TransportError> {
        let client = Client::builder()
            .connect_timeout(policy.connect_timeout())
            .timeout(policy.request_timeout())
            .redirect(Policy::none())
            .user_agent("replikans-market-readonly/0.1")
            .build()
            .map_err(|error| TransportError::ClientBuild(error.to_string()))?;

        Ok(Self { client, policy })
    }

    #[must_use]
    pub const fn policy(&self) -> &HttpPolicy {
        &self.policy
    }
}

impl HttpTransport for ReqwestHttpTransport {
    fn get(&self, endpoint: &str) -> Result<HttpResponse, TransportError> {
        let url = self.policy.parse_endpoint(endpoint)?;
        let response = self
            .client
            .get(url)
            .send()
            .map_err(|error| TransportError::Request(error.to_string()))?;

        let status = response.status().as_u16();
        if !(200..=299).contains(&status) {
            return Err(TransportError::HttpStatus(status));
        }

        if let Some(content_length) = response.content_length() {
            let max = u64::try_from(self.policy.max_response_bytes)
                .map_err(|_| TransportError::ResponseTooLarge)?;
            if content_length > max {
                return Err(TransportError::ResponseTooLarge);
            }
        }

        let read_limit = self
            .policy
            .max_response_bytes
            .checked_add(1)
            .ok_or(TransportError::ResponseTooLarge)?;
        let read_limit = u64::try_from(read_limit).map_err(|_| TransportError::ResponseTooLarge)?;
        let mut reader = response.take(read_limit);
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .map_err(|error| TransportError::Read(error.to_string()))?;
        let body = bounded_utf8_body(bytes, self.policy.max_response_bytes)?;

        Ok(HttpResponse { status, body })
    }
}

pub struct MarketPriceHttpClient<T> {
    transport: T,
}

impl<T> MarketPriceHttpClient<T>
where
    T: HttpTransport,
{
    #[must_use]
    pub const fn new(transport: T) -> Self {
        Self { transport }
    }

    #[must_use]
    pub const fn transport(&self) -> &T {
        &self.transport
    }

    pub fn fetch_price<A>(
        &self,
        adapter: &A,
        observed_at_unix_ms: u64,
        valid_until_unix_ms: u64,
        confidence: BasisPoints,
        evidence: Vec<EvidenceRef>,
    ) -> Result<PriceObservation, FetchError>
    where
        A: PublicPriceAdapter,
    {
        let response = self
            .transport
            .get(&adapter.endpoint())
            .map_err(FetchError::Transport)?;
        if !(200..=299).contains(&response.status) {
            return Err(FetchError::Transport(TransportError::HttpStatus(
                response.status,
            )));
        }

        adapter
            .parse_response(
                &response.body,
                observed_at_unix_ms,
                valid_until_unix_ms,
                confidence,
                evidence,
            )
            .map_err(FetchError::Feed)
    }
}

fn normalize_host(host: &str) -> Result<String, TransportError> {
    let host = host.trim().to_ascii_lowercase();
    if host.is_empty()
        || host.starts_with('.')
        || host.ends_with('.')
        || !host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.'))
    {
        return Err(TransportError::InvalidAllowedHost);
    }
    Ok(host)
}

fn bounded_utf8_body(bytes: Vec<u8>, max_response_bytes: usize) -> Result<String, TransportError> {
    if bytes.len() > max_response_bytes {
        return Err(TransportError::ResponseTooLarge);
    }
    String::from_utf8(bytes).map_err(|_| TransportError::NonUtf8Response)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransportError {
    InvalidPolicy(&'static str),
    InvalidAllowedHost,
    InvalidUrl,
    HttpsRequired,
    CredentialsForbidden,
    FragmentForbidden,
    MissingHost,
    NonDefaultPortForbidden,
    HostForbidden(String),
    ClientBuild(String),
    Request(String),
    HttpStatus(u16),
    ResponseTooLarge,
    NonUtf8Response,
    Read(String),
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPolicy(reason) => write!(f, "invalid HTTP policy: {reason}"),
            Self::InvalidAllowedHost => write!(f, "invalid allowed HTTP host"),
            Self::InvalidUrl => write!(f, "invalid market endpoint URL"),
            Self::HttpsRequired => write!(f, "market endpoints must use HTTPS"),
            Self::CredentialsForbidden => write!(f, "credentials are forbidden in market URLs"),
            Self::FragmentForbidden => write!(f, "URL fragments are forbidden in market URLs"),
            Self::MissingHost => write!(f, "market endpoint URL has no host"),
            Self::NonDefaultPortForbidden => {
                write!(f, "market endpoints may only use the default HTTPS port")
            }
            Self::HostForbidden(host) => write!(f, "market endpoint host is not allowed: {host}"),
            Self::ClientBuild(error) => write!(f, "failed to build HTTP client: {error}"),
            Self::Request(error) => write!(f, "market HTTP request failed: {error}"),
            Self::HttpStatus(status) => write!(f, "market HTTP request returned status {status}"),
            Self::ResponseTooLarge => write!(f, "market HTTP response exceeded size limit"),
            Self::NonUtf8Response => write!(f, "market HTTP response was not UTF-8"),
            Self::Read(error) => write!(f, "failed reading market HTTP response: {error}"),
        }
    }
}

impl std::error::Error for TransportError {}

#[derive(Debug)]
pub enum FetchError {
    Transport(TransportError),
    Feed(FeedError),
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => write!(f, "market transport failed: {error}"),
            Self::Feed(error) => write!(f, "market feed parsing failed: {error}"),
        }
    }
}

impl std::error::Error for FetchError {}

#[cfg(test)]
mod tests {
    use super::*;
    use replikan_core::Money;
    use replikan_market_feeds::CoinbaseExchangePriceAdapter;

    struct FakeTransport {
        response: HttpResponse,
    }

    impl HttpTransport for FakeTransport {
        fn get(&self, _endpoint: &str) -> Result<HttpResponse, TransportError> {
            Ok(self.response.clone())
        }
    }

    struct FailingTransport;

    impl HttpTransport for FailingTransport {
        fn get(&self, _endpoint: &str) -> Result<HttpResponse, TransportError> {
            Err(TransportError::Request("offline".to_owned()))
        }
    }

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
    fn default_policy_allows_only_documented_exchange_hosts() {
        let policy = match HttpPolicy::public_exchange_defaults() {
            Ok(value) => value,
            Err(error) => unreachable!("valid default policy: {error}"),
        };

        assert!(
            policy
                .validate_endpoint("https://api.exchange.coinbase.com/products/BTC-USD/ticker")
                .is_ok()
        );
        assert!(
            policy
                .validate_endpoint("https://api.kraken.com/0/public/Ticker?pair=XBTUSD")
                .is_ok()
        );
        assert!(matches!(
            policy.validate_endpoint("http://api.kraken.com/0/public/Ticker?pair=XBTUSD"),
            Err(TransportError::HttpsRequired)
        ));
        assert!(matches!(
            policy.validate_endpoint("https://example.com/market"),
            Err(TransportError::HostForbidden(_))
        ));
        assert!(matches!(
            policy.validate_endpoint("https://api.kraken.com:444/market"),
            Err(TransportError::NonDefaultPortForbidden)
        ));
    }

    #[test]
    fn policy_rejects_url_credentials_and_fragments() {
        let policy = match HttpPolicy::public_exchange_defaults() {
            Ok(value) => value,
            Err(error) => unreachable!("valid default policy: {error}"),
        };

        assert!(matches!(
            policy.validate_endpoint("https://user:pass@api.kraken.com/market"),
            Err(TransportError::CredentialsForbidden)
        ));
        assert!(matches!(
            policy.validate_endpoint("https://api.kraken.com/market#secret"),
            Err(TransportError::FragmentForbidden)
        ));
    }

    #[test]
    fn bounded_body_rejects_oversized_and_non_utf8_payloads() {
        assert!(matches!(
            bounded_utf8_body(vec![b'a'; 5], 4),
            Err(TransportError::ResponseTooLarge)
        ));
        assert!(matches!(
            bounded_utf8_body(vec![0xff], 4),
            Err(TransportError::NonUtf8Response)
        ));
    }

    #[test]
    fn fetcher_combines_transport_and_feed_parser_without_custody() {
        let transport = FakeTransport {
            response: HttpResponse {
                status: 200,
                body: r#"{"trade_id":1,"price":"6268.48123499"}"#.to_owned(),
            },
        };
        let client = MarketPriceHttpClient::new(transport);
        let adapter = match CoinbaseExchangePriceAdapter::new("BTC-USD", "BTC") {
            Ok(value) => value,
            Err(error) => unreachable!("valid adapter: {error}"),
        };
        let observation = match client.fetch_price(
            &adapter,
            1_000_000,
            1_060_000,
            bps(9_000),
            vec![evidence("coinbase:http:test")],
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("valid fetched price: {error}"),
        };

        assert_eq!(
            observation.price_per_unit,
            Money::from_micros(6_268_481_234)
        );
        assert_eq!(observation.source_id, "coinbase-exchange");
    }

    #[test]
    fn fetcher_preserves_transport_failure_boundary() {
        let client = MarketPriceHttpClient::new(FailingTransport);
        let adapter = match CoinbaseExchangePriceAdapter::new("BTC-USD", "BTC") {
            Ok(value) => value,
            Err(error) => unreachable!("valid adapter: {error}"),
        };
        let result = client.fetch_price(
            &adapter,
            1,
            2,
            bps(9_000),
            vec![evidence("coinbase:http:error")],
        );

        assert!(matches!(
            result,
            Err(FetchError::Transport(TransportError::Request(_)))
        ));
    }
}
