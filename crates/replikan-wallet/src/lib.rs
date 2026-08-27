#![forbid(unsafe_code)]

use core::fmt;
use replikan_core::PublicIdentity;

/// A domain-separated request to authorize bytes.
///
/// The caller can request a signature but has no API for reading secret key material.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SigningRequest {
    pub domain: String,
    pub payload: Vec<u8>,
}

impl SigningRequest {
    pub fn new(domain: impl Into<String>, payload: impl Into<Vec<u8>>) -> Result<Self, SigningRequestError> {
        let domain = domain.into();
        if domain.trim().is_empty() {
            return Err(SigningRequestError::EmptyDomain);
        }
        Ok(Self {
            domain,
            payload: payload.into(),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SigningRequestError {
    EmptyDomain,
}

impl fmt::Display for SigningRequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "signing request domain cannot be empty")
    }
}

impl std::error::Error for SigningRequestError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Signature(Vec<u8>);

impl Signature {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Result<Self, SignatureError> {
        let bytes = bytes.into();
        if bytes.is_empty() {
            Err(SignatureError::Empty)
        } else {
            Ok(Self(bytes))
        }
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SignatureError {
    Empty,
}

impl fmt::Display for SignatureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "signature cannot be empty")
    }
}

impl std::error::Error for SignatureError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SignerError {
    Locked,
    PolicyDenied(String),
    Backend(String),
}

impl fmt::Display for SignerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Locked => write!(f, "signer is locked"),
            Self::PolicyDenied(reason) => write!(f, "signing denied by policy: {reason}"),
            Self::Backend(reason) => write!(f, "signer backend failed: {reason}"),
        }
    }
}

impl std::error::Error for SignerError {}

/// Capability boundary for cryptographic custody.
///
/// There is intentionally no method to export a private key or seed phrase.
pub trait Signer: Send + Sync {
    fn public_identity(&self) -> &PublicIdentity;
    fn sign(&self, request: &SigningRequest) -> Result<Signature, SignerError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct InMemoryTestSigner {
        identity: PublicIdentity,
    }

    impl Signer for InMemoryTestSigner {
        fn public_identity(&self) -> &PublicIdentity {
            &self.identity
        }

        fn sign(&self, request: &SigningRequest) -> Result<Signature, SignerError> {
            let mut bytes = request.domain.as_bytes().to_vec();
            bytes.extend_from_slice(&request.payload);
            Signature::new(bytes).map_err(|error| SignerError::Backend(error.to_string()))
        }
    }

    #[test]
    fn signer_exposes_public_identity_but_not_secret_material() {
        let identity = match PublicIdentity::new("test-runtime-identity") {
            Ok(value) => value,
            Err(error) => unreachable!("valid public identity: {error}"),
        };
        let signer = InMemoryTestSigner { identity };
        let request = match SigningRequest::new("replikans.test", b"payload".to_vec()) {
            Ok(value) => value,
            Err(error) => unreachable!("valid signing request: {error}"),
        };
        let signature = match signer.sign(&request) {
            Ok(value) => value,
            Err(error) => unreachable!("test signer should sign: {error}"),
        };

        assert_eq!(signer.public_identity().as_str(), "test-runtime-identity");
        assert!(!signature.as_bytes().is_empty());
    }

    #[test]
    fn empty_domain_is_rejected() {
        assert_eq!(
            SigningRequest::new("   ", Vec::<u8>::new()),
            Err(SigningRequestError::EmptyDomain)
        );
    }
}
