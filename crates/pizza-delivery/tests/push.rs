mod harness;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;
use harness::{Harness, TestResult};
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_128_GCM};
use ring::agreement::{agree_ephemeral, EphemeralPrivateKey, UnparsedPublicKey, ECDH_P256};
use ring::hkdf::{KeyType, Salt, HKDF_SHA256};
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{UnparsedPublicKey as Verifying, ECDSA_P256_SHA256_FIXED};
use serde_json::json;

struct Len(usize);

impl KeyType for Len {
    fn len(&self) -> usize {
        self.0
    }
}

struct Captured {
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn push_service() -> TestResult<(String, mpsc::Receiver<Captured>)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let endpoint = format!(
        "http://127.0.0.1:{}/push/device-1",
        listener.local_addr()?.port()
    );
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        while let Ok((mut stream, _)) = listener.accept() {
            let mut raw = Vec::new();
            let mut buf = [0u8; 4096];
            let split = loop {
                let Ok(n) = stream.read(&mut buf) else { return };
                if n == 0 {
                    return;
                }
                raw.extend_from_slice(&buf[..n]);
                if let Some(at) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                    break at + 4;
                }
            };
            let head = String::from_utf8_lossy(&raw[..split]).into_owned();
            let headers: Vec<(String, String)> = head
                .lines()
                .skip(1)
                .filter_map(|l| l.split_once(':'))
                .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
                .collect();
            let length: usize = headers
                .iter()
                .find(|(k, _)| k == "content-length")
                .and_then(|(_, v)| v.parse().ok())
                .unwrap_or(0);
            while raw.len() < split + length {
                let Ok(n) = stream.read(&mut buf) else { return };
                if n == 0 {
                    break;
                }
                raw.extend_from_slice(&buf[..n]);
            }
            let _ = stream.write_all(
                b"HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            let _ = tx.send(Captured {
                headers,
                body: raw[split..].to_vec(),
            });
        }
    });
    Ok((endpoint, rx))
}

fn expand(prk: &ring::hkdf::Prk, info: &[u8], out: &mut [u8]) -> TestResult {
    prk.expand(&[info], Len(out.len()))
        .and_then(|okm| okm.fill(out))
        .map_err(|_| "hkdf")?;
    Ok(())
}

fn decrypt(
    body: &[u8],
    private: EphemeralPrivateKey,
    ua_public: &[u8],
    auth: &[u8],
) -> TestResult<Vec<u8>> {
    let salt = &body[..16];
    let id_len = body[20] as usize;
    let sender = &body[21..21 + id_len];
    let record = &body[21 + id_len..];
    let secret = agree_ephemeral(private, &UnparsedPublicKey::new(&ECDH_P256, sender), |s| {
        s.to_vec()
    })
    .map_err(|_| "agree")?;
    let mut info = b"WebPush: info\0".to_vec();
    info.extend_from_slice(ua_public);
    info.extend_from_slice(sender);
    let mut ikm = [0u8; 32];
    expand(
        &Salt::new(HKDF_SHA256, auth).extract(&secret),
        &info,
        &mut ikm,
    )?;
    let prk = Salt::new(HKDF_SHA256, salt).extract(&ikm);
    let (mut cek, mut nonce) = ([0u8; 16], [0u8; 12]);
    expand(&prk, b"Content-Encoding: aes128gcm\0", &mut cek)?;
    expand(&prk, b"Content-Encoding: nonce\0", &mut nonce)?;
    let key = LessSafeKey::new(UnboundKey::new(&AES_128_GCM, &cek).map_err(|_| "key")?);
    let mut buffer = record.to_vec();
    let plain = key
        .open_in_place(
            Nonce::assume_unique_for_key(nonce),
            Aad::empty(),
            &mut buffer,
        )
        .map_err(|_| "decrypt")?;
    let end = plain.iter().rposition(|b| *b != 0).ok_or("empty")?;
    assert_eq!(plain[end], 2);
    Ok(plain[..end].to_vec())
}

#[test]
fn a_finished_job_pushes_an_encrypted_minimal_notification_to_subscribers() -> TestResult {
    let h = Harness::new()?;
    let (endpoint, received) = push_service()?;
    let rng = SystemRandom::new();
    let private = EphemeralPrivateKey::generate(&ECDH_P256, &rng).map_err(|_| "keygen")?;
    let ua_public = private
        .compute_public_key()
        .map_err(|_| "pub")?
        .as_ref()
        .to_vec();
    let mut auth = [0u8; 16];
    rng.fill(&mut auth).map_err(|_| "rng")?;

    let key = h.get("/api/push/key")?["key"]
        .as_str()
        .ok_or("no key")?
        .to_owned();
    assert_eq!(B64.decode(&key)?.len(), 65);
    let (status, _) = h.http(
        "POST",
        "/api/push/subscribe",
        Some(&json!({"endpoint": endpoint, "keys": {"p256dh": B64.encode(&ua_public), "auth": B64.encode(auth)}})),
    )?;
    assert_eq!(status, 200);
    let (status, _) = h.http(
        "POST",
        "/api/push/subscribe",
        Some(
            &json!({"endpoint": "http://169.254.169.254/x", "keys": {"p256dh": "a", "auth": "b"}}),
        ),
    )?;
    assert_eq!(status, 400);

    let short = h.submit_in_repo("a very private prompt")?;
    h.wait_for_end(&short)?;
    let job = h.get(&format!("/api/jobs/{short}"))?["job"]["id"]
        .as_str()
        .ok_or("no id")?
        .to_owned();

    let push = received.recv_timeout(Duration::from_secs(10))?;
    let header = |name: &str| {
        push.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };
    assert_eq!(header("content-encoding"), Some("aes128gcm"));
    assert_eq!(header("urgency"), Some("normal"));
    let authorization = header("authorization").ok_or("no vapid header")?;
    let token = authorization
        .strip_prefix("vapid t=")
        .and_then(|rest| rest.split_once(", k="))
        .ok_or("bad vapid header")?;
    assert_eq!(token.1, key);
    let (signed, signature) = token.0.rsplit_once('.').ok_or("bad jwt")?;
    Verifying::new(&ECDSA_P256_SHA256_FIXED, B64.decode(&key)?)
        .verify(signed.as_bytes(), &B64.decode(signature)?)
        .map_err(|_| "vapid signature")?;

    let plain = decrypt(&push.body, private, &ua_public, &auth)?;
    let payload: serde_json::Value = serde_json::from_slice(&plain)?;
    assert_eq!(
        payload,
        json!({"job": job, "state": "completed", "title": "Job completed", "url": format!("/jobs/{job}")})
    );
    assert!(!String::from_utf8_lossy(&plain).contains("private"));
    assert!(received.recv_timeout(Duration::from_millis(500)).is_err());
    Ok(())
}
