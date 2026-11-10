use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;
use ring::signature::{RsaPublicKeyComponents, RSA_PKCS1_2048_8192_SHA256};
use serde::Deserialize;

use crate::config::RemoteConfig;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Jwk {
    pub kid: String,
    pub n: Vec<u8>,
    pub e: Vec<u8>,
}

pub trait KeySource: Send + Sync {
    fn fetch(&self) -> Result<Vec<Jwk>, String>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Reject {
    Missing,
    Malformed,
    Algorithm,
    UnknownKey,
    BadSignature,
    Audience,
    Issuer,
    Expired,
    NotAllowed,
    KeysUnavailable,
    Disabled,
}

impl Reject {
    pub fn as_str(self) -> &'static str {
        match self {
            Reject::Missing => "missing-assertion",
            Reject::Malformed => "malformed-assertion",
            Reject::Algorithm => "unsupported-algorithm",
            Reject::UnknownKey => "unknown-key",
            Reject::BadSignature => "bad-signature",
            Reject::Audience => "wrong-audience",
            Reject::Issuer => "wrong-issuer",
            Reject::Expired => "expired",
            Reject::NotAllowed => "email-not-allowed",
            Reject::KeysUnavailable => "keys-unavailable",
            Reject::Disabled => "remote-disabled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub email: String,
}

struct Cache {
    keys: Vec<Jwk>,
    fetched: Option<Instant>,
}

pub struct Verifier {
    audience: String,
    issuer: String,
    emails: Vec<String>,
    source: Box<dyn KeySource>,
    refresh_min: Duration,
    cache: Mutex<Cache>,
}

#[derive(Deserialize)]
struct Header {
    alg: String,
    kid: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

#[derive(Deserialize)]
struct Claims {
    aud: Option<Audience>,
    iss: Option<String>,
    exp: Option<i64>,
    nbf: Option<i64>,
    email: Option<String>,
}

impl Verifier {
    pub fn new(config: &RemoteConfig, source: Box<dyn KeySource>) -> Self {
        Self {
            audience: config.audience.clone(),
            issuer: config.issuer.clone(),
            emails: config.emails.clone(),
            source,
            refresh_min: Duration::from_millis(config.refresh_min_ms),
            cache: Mutex::new(Cache {
                keys: Vec::new(),
                fetched: None,
            }),
        }
    }

    pub fn verify(&self, token: Option<&str>, now_secs: i64) -> Result<Identity, Reject> {
        let token = token
            .filter(|t| !t.trim().is_empty())
            .ok_or(Reject::Missing)?;
        let mut parts = token.trim().split('.');
        let (Some(head), Some(body), Some(sig), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(Reject::Malformed);
        };
        let header: Header = decode(head)?;
        if header.alg != "RS256" {
            return Err(Reject::Algorithm);
        }
        let kid = header.kid.ok_or(Reject::Malformed)?;
        let signature = B64.decode(sig).map_err(|_| Reject::Malformed)?;
        let key = self.key(&kid)?;
        RsaPublicKeyComponents {
            n: &key.n,
            e: &key.e,
        }
        .verify(
            &RSA_PKCS1_2048_8192_SHA256,
            format!("{head}.{body}").as_bytes(),
            &signature,
        )
        .map_err(|_| Reject::BadSignature)?;
        let claims: Claims = decode(body)?;
        let audience_ok = match claims.aud {
            Some(Audience::One(a)) => a == self.audience,
            Some(Audience::Many(list)) => list.contains(&self.audience),
            None => false,
        };
        if !audience_ok {
            return Err(Reject::Audience);
        }
        if claims.iss.as_deref().map(|i| i.trim_end_matches('/')) != Some(self.issuer.as_str()) {
            return Err(Reject::Issuer);
        }
        match claims.exp {
            Some(exp) if exp > now_secs => {}
            _ => return Err(Reject::Expired),
        }
        if claims.nbf.is_some_and(|nbf| nbf > now_secs) {
            return Err(Reject::Expired);
        }
        let email = claims.email.ok_or(Reject::NotAllowed)?.to_ascii_lowercase();
        if !self.emails.contains(&email) {
            return Err(Reject::NotAllowed);
        }
        Ok(Identity { email })
    }

    fn key(&self, kid: &str) -> Result<Jwk, Reject> {
        let mut cache = self.cache.lock().map_err(|_| Reject::KeysUnavailable)?;
        if let Some(key) = cache.keys.iter().find(|k| k.kid == kid) {
            return Ok(key.clone());
        }
        let due = cache
            .fetched
            .map_or(true, |at| at.elapsed() >= self.refresh_min);
        if !due {
            return Err(Reject::UnknownKey);
        }
        cache.fetched = Some(Instant::now());
        cache.keys = self.source.fetch().map_err(|_| Reject::KeysUnavailable)?;
        cache
            .keys
            .iter()
            .find(|k| k.kid == kid)
            .cloned()
            .ok_or(Reject::UnknownKey)
    }
}

fn decode<T: for<'de> Deserialize<'de>>(segment: &str) -> Result<T, Reject> {
    let bytes = B64.decode(segment).map_err(|_| Reject::Malformed)?;
    serde_json::from_slice(&bytes).map_err(|_| Reject::Malformed)
}

pub struct HttpKeySource {
    url: String,
}

impl HttpKeySource {
    pub fn new(url: String) -> Self {
        Self { url }
    }
}

#[derive(Deserialize)]
struct Jwks {
    keys: Vec<RawJwk>,
}

#[derive(Deserialize)]
struct RawJwk {
    kid: String,
    kty: String,
    n: String,
    e: String,
}

impl KeySource for HttpKeySource {
    fn fetch(&self) -> Result<Vec<Jwk>, String> {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(5))
            .build();
        let body = agent
            .get(&self.url)
            .call()
            .map_err(|e| e.to_string())?
            .into_string()
            .map_err(|e| e.to_string())?;
        let jwks: Jwks = serde_json::from_str(&body).map_err(|e| e.to_string())?;
        jwks.keys
            .into_iter()
            .filter(|k| k.kty == "RSA")
            .map(|k| {
                Ok(Jwk {
                    kid: k.kid,
                    n: B64.decode(k.n).map_err(|e| e.to_string())?,
                    e: B64.decode(k.e).map_err(|e| e.to_string())?,
                })
            })
            .collect()
    }
}
