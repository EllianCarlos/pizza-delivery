#![allow(dead_code, clippy::expect_used)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;
use ring::rand::SystemRandom;
use ring::signature::{RsaKeyPair, RSA_PKCS1_SHA256};

pub const AUD: &str = "test-audience";
pub const ISSUER: &str = "https://team.cloudflareaccess.com";
pub const OWNER: &str = "owner@example.com";

pub struct Signer {
    pair: RsaKeyPair,
}

impl Signer {
    pub fn one() -> Self {
        Self::from(include_bytes!("../fixtures/access-signer-1.der"))
    }

    pub fn two() -> Self {
        Self::from(include_bytes!("../fixtures/access-signer-2.der"))
    }

    fn from(der: &[u8]) -> Self {
        Self {
            pair: RsaKeyPair::from_der(der).expect("fixture key"),
        }
    }

    pub fn jwk(&self, kid: &str) -> serde_json::Value {
        let public: ring::rsa::PublicKeyComponents<Vec<u8>> = self.pair.public().into();
        serde_json::json!({
            "kty": "RSA", "alg": "RS256", "use": "sig", "kid": kid,
            "n": B64.encode(&public.n),
            "e": B64.encode(&public.e),
        })
    }

    pub fn sign(&self, kid: &str, claims: &serde_json::Value) -> String {
        self.sign_with_alg(kid, "RS256", claims)
    }

    pub fn sign_with_alg(&self, kid: &str, alg: &str, claims: &serde_json::Value) -> String {
        let header = serde_json::json!({"alg": alg, "kid": kid, "typ": "JWT"});
        let signing_input = format!(
            "{}.{}",
            B64.encode(header.to_string()),
            B64.encode(claims.to_string())
        );
        let mut signature = vec![0u8; self.pair.public().modulus_len()];
        self.pair
            .sign(
                &RSA_PKCS1_SHA256,
                &SystemRandom::new(),
                signing_input.as_bytes(),
                &mut signature,
            )
            .expect("sign");
        format!("{signing_input}.{}", B64.encode(signature))
    }
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn claims(email: &str) -> serde_json::Value {
    serde_json::json!({
        "aud": [AUD], "iss": ISSUER, "email": email,
        "iat": now(), "exp": now() + 3600,
    })
}

pub struct CertsServer {
    pub url: String,
    keys: Arc<Mutex<String>>,
    hits: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
}

impl CertsServer {
    pub fn start(keys: &[(&str, &Signer)]) -> Result<Self, Box<dyn std::error::Error>> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let url = format!(
            "http://127.0.0.1:{}/cdn-cgi/access/certs",
            listener.local_addr()?.port()
        );
        let server = Self {
            url,
            keys: Arc::new(Mutex::new(jwks(keys))),
            hits: Arc::new(AtomicUsize::new(0)),
            stop: Arc::new(AtomicBool::new(false)),
        };
        let (body, hits, stop) = (
            server.keys.clone(),
            server.hits.clone(),
            server.stop.clone(),
        );
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    continue;
                };
                let _ = stream.set_nonblocking(false);
                let mut buf = [0u8; 2048];
                let _ = stream.read(&mut buf);
                hits.fetch_add(1, Ordering::SeqCst);
                let payload = body.lock().map(|b| b.clone()).unwrap_or_default();
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                    payload.len()
                );
            }
        });
        Ok(server)
    }

    pub fn rotate(&self, keys: &[(&str, &Signer)]) {
        if let Ok(mut guard) = self.keys.lock() {
            *guard = jwks(keys);
        }
    }

    pub fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

impl Drop for CertsServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn jwks(keys: &[(&str, &Signer)]) -> String {
    let list: Vec<_> = keys.iter().map(|(kid, s)| s.jwk(kid)).collect();
    serde_json::json!({"keys": list}).to_string()
}
