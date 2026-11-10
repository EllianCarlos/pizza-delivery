use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::lock::acquire;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;
use pizza_delivery_core::notify::{Notification, Notifier, NotifyError, NotifyKind, Urgency};
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_128_GCM};
use ring::agreement::{agree_ephemeral, EphemeralPrivateKey, UnparsedPublicKey, ECDH_P256};
use ring::hkdf::{KeyType, Prk, Salt, HKDF_SHA256};
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};
use serde::{Deserialize, Serialize};

const RECORD_SIZE: u32 = 4096;
const VAPID_LIFETIME_SECS: u64 = 12 * 60 * 60;
const PUSH_TTL_SECS: u64 = 24 * 60 * 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscription {
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
}

pub struct Vapid {
    pair: EcdsaKeyPair,
    public: Vec<u8>,
}

impl Vapid {
    pub fn load_or_create(path: &Path) -> Result<Self, String> {
        let rng = SystemRandom::new();
        let pkcs8 = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let generated =
                    EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
                        .map_err(|e| e.to_string())?;
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                write_private(path, generated.as_ref())?;
                generated.as_ref().to_vec()
            }
            Err(e) => return Err(e.to_string()),
        };
        let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &pkcs8, &rng)
            .map_err(|e| e.to_string())?;
        let public = pair.public_key().as_ref().to_vec();
        Ok(Self { pair, public })
    }

    pub fn public_key(&self) -> String {
        B64.encode(&self.public)
    }

    pub fn authorization(
        &self,
        endpoint: &str,
        subject: &str,
        now_secs: u64,
    ) -> Result<String, String> {
        let audience = origin_of(endpoint).ok_or("endpoint has no origin")?;
        let header = B64.encode(r#"{"typ":"JWT","alg":"ES256"}"#);
        let claims = B64.encode(
            serde_json::json!({
                "aud": audience,
                "exp": now_secs + VAPID_LIFETIME_SECS,
                "sub": subject,
            })
            .to_string(),
        );
        let input = format!("{header}.{claims}");
        let signature = self
            .pair
            .sign(&SystemRandom::new(), input.as_bytes())
            .map_err(|e| e.to_string())?;
        Ok(format!(
            "vapid t={input}.{}, k={}",
            B64.encode(signature.as_ref()),
            self.public_key()
        ))
    }
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(bytes).map_err(|e| e.to_string())
}

pub fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    (!authority.is_empty()).then(|| format!("{scheme}://{authority}"))
}

pub fn endpoint_allowed(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .rsplit_once(':')
        .map_or_else(|| rest.split('/').next().unwrap_or(""), |(h, _)| h);
    match scheme {
        "https" => !host.is_empty(),
        "http" => matches!(host, "127.0.0.1" | "localhost"),
        _ => false,
    }
}

struct Len(usize);

impl KeyType for Len {
    fn len(&self) -> usize {
        self.0
    }
}

fn expand(prk: &Prk, info: &[u8], out: &mut [u8]) -> Result<(), String> {
    prk.expand(&[info], Len(out.len()))
        .and_then(|okm| okm.fill(out))
        .map_err(|_| "hkdf expand failed".to_owned())
}

pub fn encrypt(payload: &[u8], p256dh: &[u8], auth: &[u8]) -> Result<Vec<u8>, String> {
    let rng = SystemRandom::new();
    let private = EphemeralPrivateKey::generate(&ECDH_P256, &rng).map_err(|e| e.to_string())?;
    let public = private.compute_public_key().map_err(|e| e.to_string())?;
    let secret = agree_ephemeral(private, &UnparsedPublicKey::new(&ECDH_P256, p256dh), |s| {
        s.to_vec()
    })
    .map_err(|_| "bad subscription key")?;

    let key_prk = Salt::new(HKDF_SHA256, auth).extract(&secret);
    let mut key_info = b"WebPush: info\0".to_vec();
    key_info.extend_from_slice(p256dh);
    key_info.extend_from_slice(public.as_ref());
    let mut ikm = [0u8; 32];
    expand(&key_prk, &key_info, &mut ikm)?;

    let mut salt = [0u8; 16];
    rng.fill(&mut salt).map_err(|e| e.to_string())?;
    let prk = Salt::new(HKDF_SHA256, &salt).extract(&ikm);
    let mut cek = [0u8; 16];
    let mut nonce = [0u8; 12];
    expand(&prk, b"Content-Encoding: aes128gcm\0", &mut cek)?;
    expand(&prk, b"Content-Encoding: nonce\0", &mut nonce)?;

    let mut record = payload.to_vec();
    record.push(2);
    let key = LessSafeKey::new(UnboundKey::new(&AES_128_GCM, &cek).map_err(|e| e.to_string())?);
    key.seal_in_place_append_tag(
        Nonce::assume_unique_for_key(nonce),
        Aad::empty(),
        &mut record,
    )
    .map_err(|e| e.to_string())?;

    let mut body = salt.to_vec();
    body.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    body.push(public.as_ref().len() as u8);
    body.extend_from_slice(public.as_ref());
    body.extend_from_slice(&record);
    Ok(body)
}

pub fn push_urgency(urgency: Urgency) -> &'static str {
    match urgency {
        Urgency::Low => "low",
        Urgency::Normal => "normal",
        Urgency::Critical => "high",
    }
}

pub fn short_title(kind: NotifyKind) -> &'static str {
    match kind {
        NotifyKind::Completed => "Job completed",
        NotifyKind::HighFeedback => "High severity feedback",
        NotifyKind::Failed => "Job failed",
        NotifyKind::NeedsHuman => "Job needs you",
        NotifyKind::QuotaParked => "Job waiting for quota",
    }
}

pub fn payload_of(n: &Notification) -> String {
    serde_json::json!({
        "job": n.job.as_str(),
        "state": n.kind.as_str(),
        "title": short_title(n.kind),
        "url": format!("/jobs/{}", n.job),
    })
    .to_string()
}

pub struct Subscriptions {
    path: PathBuf,
    items: Mutex<Vec<Subscription>>,
}

impl Subscriptions {
    pub fn open(path: PathBuf) -> Self {
        let items = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            path,
            items: Mutex::new(items),
        }
    }

    pub fn list(&self) -> Vec<Subscription> {
        self.items.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn add(&self, subscription: Subscription) -> Result<(), String> {
        let mut guard = acquire(&self.items, "items")?;
        guard.retain(|s| s.endpoint != subscription.endpoint);
        guard.push(subscription);
        self.save(&guard)
    }

    pub fn remove(&self, endpoint: &str) -> Result<(), String> {
        let mut guard = acquire(&self.items, "items")?;
        guard.retain(|s| s.endpoint != endpoint);
        self.save(&guard)
    }

    fn save(&self, items: &[Subscription]) -> Result<(), String> {
        let text = serde_json::to_vec(items).map_err(|e| e.to_string())?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&self.path, text).map_err(|e| e.to_string())
    }
}

pub struct Push {
    pub vapid: Vapid,
    pub subscriptions: Subscriptions,
    pub subject: String,
}

pub struct WebPushNotifier {
    push: Arc<Push>,
}

impl WebPushNotifier {
    pub fn new(push: Arc<Push>) -> Self {
        Self { push }
    }
}

fn deliver(push: &Push, subscription: &Subscription, n: &Notification) -> Result<u16, String> {
    let p256dh = B64
        .decode(&subscription.p256dh)
        .map_err(|e| e.to_string())?;
    let auth = B64.decode(&subscription.auth).map_err(|e| e.to_string())?;
    let body = encrypt(payload_of(n).as_bytes(), &p256dh, &auth)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let authorization = push
        .vapid
        .authorization(&subscription.endpoint, &push.subject, now)?;
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(15))
        .build();
    match agent
        .post(&subscription.endpoint)
        .set("Authorization", &authorization)
        .set("Content-Encoding", "aes128gcm")
        .set("Content-Type", "application/octet-stream")
        .set("TTL", &PUSH_TTL_SECS.to_string())
        .set("Urgency", push_urgency(n.urgency))
        .send_bytes(&body)
    {
        Ok(response) => Ok(response.status()),
        Err(ureq::Error::Status(code, _)) => Ok(code),
        Err(e) => Err(e.to_string()),
    }
}

impl Notifier for WebPushNotifier {
    fn notify(&self, n: &Notification) -> Result<(), NotifyError> {
        let subscriptions = self.push.subscriptions.list();
        if subscriptions.is_empty() {
            return Ok(());
        }
        let (push, n) = (Arc::clone(&self.push), n.clone());
        std::thread::spawn(move || {
            for subscription in subscriptions {
                match deliver(&push, &subscription, &n) {
                    Ok(404 | 410) => {
                        let _ = push.subscriptions.remove(&subscription.endpoint);
                    }
                    Ok(code) if code >= 400 => eprintln!("web push: status {code}"),
                    Ok(_) => {}
                    Err(e) => eprintln!("web push: {e}"),
                }
            }
        });
        Ok(())
    }
}

pub struct FanOut(pub Vec<Box<dyn Notifier>>);

impl Notifier for FanOut {
    fn notify(&self, n: &Notification) -> Result<(), NotifyError> {
        let mut first = None;
        for notifier in &self.0 {
            if let Err(e) = notifier.notify(n) {
                first.get_or_insert(e);
            }
        }
        first.map_or(Ok(()), Err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urgency_maps_to_the_web_push_scale() {
        assert_eq!(push_urgency(Urgency::Low), "low");
        assert_eq!(push_urgency(Urgency::Normal), "normal");
        assert_eq!(push_urgency(Urgency::Critical), "high");
    }

    #[test]
    fn only_loopback_may_use_plain_http() {
        assert!(endpoint_allowed("https://push.example.com/abc"));
        assert!(endpoint_allowed("http://127.0.0.1:9000/abc"));
        assert!(!endpoint_allowed("http://169.254.169.254/latest"));
        assert!(!endpoint_allowed("file:///etc/passwd"));
    }

    #[test]
    fn vapid_keys_are_generated_once_and_reused() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("vapid.pk8");
        let first = Vapid::load_or_create(&path)?;
        let second = Vapid::load_or_create(&path)?;
        assert_eq!(first.public_key(), second.public_key());
        assert_eq!(B64.decode(first.public_key())?.len(), 65);
        Ok(())
    }

    #[test]
    fn the_origin_is_the_vapid_audience() {
        assert_eq!(
            origin_of("https://fcm.googleapis.com/fcm/send/abc?x=1"),
            Some("https://fcm.googleapis.com".to_owned())
        );
    }
}
