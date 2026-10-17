//! Outbound HTTP calls to a peer's CrossPaste server.
//!
//! Every request carries `appInstanceId` (ours). Calls that address a known
//! peer also carry `targetAppInstanceId`; with `secure: 1` the request body is
//! encrypted and the peer encrypts its successful response (JSON whole,
//! binary as length-prefixed records).

use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};

use crate::crypto::MessageCipher;
use crate::model::{
    error_code, FailResponse, KeyExchange, PasteData, PullFileRequest, SyncInfo, TrustConfirm,
};

pub const HEADER_APP_INSTANCE_ID: &str = "appInstanceId";
pub const HEADER_TARGET_APP_INSTANCE_ID: &str = "targetAppInstanceId";
pub const HEADER_SECURE: &str = "secure";
pub const HEADER_EXCHANGE_TIMESTAMP: &str = "crosspaste-exchange-timestamp";

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("network: {0}")]
    Net(#[from] reqwest::Error),
    #[error("peer error {code}: {message}")]
    Peer {
        status: u16,
        code: i32,
        message: String,
    },
    #[error("HTTP {0}")]
    Status(u16),
    #[error("bad response: {0}")]
    Bad(String),
}

impl ClientError {
    pub fn peer_code(&self) -> Option<i32> {
        match self {
            ClientError::Peer { code, .. } => Some(*code),
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, ClientError>;

/// Result of `GET /sync/telnet`.
pub struct Telnet {
    pub version: i32,
    pub app_instance_id: Option<String>,
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    own_id: String,
}

fn url(host: &str, port: u16, path: &str) -> String {
    format!("http://{host}:{port}{path}")
}

impl Client {
    pub fn new(own_id: String) -> Self {
        let http = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(2))
            .build()
            .expect("build HTTP client");
        Self { http, own_id }
    }

    fn headers(&self, target: Option<&str>, secure: bool) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            HEADER_APP_INSTANCE_ID,
            HeaderValue::from_str(&self.own_id).unwrap(),
        );
        h.insert("Accept-Encoding", HeaderValue::from_static("identity"));
        if let Some(t) = target {
            if let Ok(v) = HeaderValue::from_str(t) {
                h.insert(HEADER_TARGET_APP_INSTANCE_ID, v);
            }
        }
        if secure {
            h.insert(HEADER_SECURE, HeaderValue::from_static("1"));
        }
        h
    }

    async fn check(resp: reqwest::Response) -> Result<reqwest::Response> {
        let status = resp.status().as_u16();
        if (200..300).contains(&status) {
            return Ok(resp);
        }
        let body = resp.bytes().await.unwrap_or_default();
        match serde_json::from_slice::<FailResponse>(&body) {
            Ok(f) => Err(ClientError::Peer {
                status,
                code: f.error_code,
                message: f.message,
            }),
            Err(_) => Err(ClientError::Status(status)),
        }
    }

    pub async fn sync_info(&self, host: &str, port: u16) -> Result<SyncInfo> {
        let resp = self
            .http
            .get(url(host, port, "/sync/syncInfo"))
            .headers(self.headers(None, false))
            .timeout(Duration::from_secs(3))
            .send()
            .await?;
        let bytes = Self::check(resp).await?.bytes().await?;
        serde_json::from_slice(&bytes).map_err(|e| ClientError::Bad(e.to_string()))
    }

    pub async fn telnet(&self, host: &str, port: u16, timeout: Duration) -> Result<Telnet> {
        let resp = self
            .http
            .get(url(host, port, "/sync/telnet"))
            .headers(self.headers(None, false))
            .timeout(timeout)
            .send()
            .await?;
        let resp = Self::check(resp).await?;
        let id = resp
            .headers()
            .get(HEADER_APP_INSTANCE_ID)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let text = resp.text().await?;
        let version = text
            .trim()
            .parse()
            .map_err(|_| ClientError::Bad(format!("telnet body {text:?}")))?;
        Ok(Telnet {
            version,
            app_instance_id: id,
        })
    }

    /// Plain liveness heartbeat; returns the peer's `SyncApi.VERSION`.
    pub async fn heartbeat(&self, host: &str, port: u16, target: &str) -> Result<i32> {
        let resp = self
            .http
            .get(url(host, port, "/sync/heartbeat"))
            .headers(self.headers(Some(target), false))
            .timeout(Duration::from_secs(3))
            .send()
            .await?;
        let text = Self::check(resp).await?.text().await?;
        text.trim()
            .parse()
            .map_err(|_| ClientError::Bad(format!("heartbeat body {text:?}")))
    }

    /// Heartbeat that also tells the peer our current address (authenticated by encryption).
    pub async fn heartbeat_with_info(
        &self,
        host: &str,
        port: u16,
        target: &str,
        cipher: &MessageCipher,
        info: &SyncInfo,
    ) -> Result<i32> {
        let body = cipher.encrypt(&serde_json::to_vec(info).unwrap());
        let resp = self
            .http
            .post(url(host, port, "/sync/heartbeat/syncInfo"))
            .headers(self.headers(Some(target), true))
            .header(CONTENT_TYPE, "application/json")
            .timeout(Duration::from_secs(3))
            .body(body)
            .send()
            .await?;
        let bytes = Self::check(resp).await?.bytes().await?;
        let plain = cipher
            .decrypt(&bytes)
            .map_err(|_| ClientError::Bad("undecryptable heartbeat".into()))?;
        let text = String::from_utf8_lossy(&plain);
        text.trim()
            .parse()
            .map_err(|_| ClientError::Bad(format!("heartbeat body {text:?}")))
    }

    pub async fn exchange(&self, host: &str, port: u16, req: &KeyExchange) -> Result<KeyExchange> {
        let resp = self
            .http
            .post(url(host, port, "/sync/trust/v2/exchange"))
            .headers(self.headers(None, false))
            .header(CONTENT_TYPE, "application/json")
            .timeout(Duration::from_secs(5))
            .body(serde_json::to_vec(req).unwrap())
            .send()
            .await?;
        let bytes = Self::check(resp).await?.bytes().await?;
        serde_json::from_slice(&bytes).map_err(|e| ClientError::Bad(e.to_string()))
    }

    /// `own_info` rides along as `crosspaste-sync-info` so the peer can
    /// register us even if it has not seen our mDNS record yet.
    pub async fn confirm(
        &self,
        host: &str,
        port: u16,
        own_host: &str,
        own_info: &SyncInfo,
        req: &TrustConfirm,
    ) -> Result<TrustConfirm> {
        use base64::Engine;
        let info_header =
            base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(own_info).unwrap());
        let resp = self
            .http
            .post(url(host, port, "/sync/trust/v2/confirm"))
            .headers(self.headers(None, false))
            .header("crosspaste-host", own_host)
            .header("crosspaste-sync-info", info_header)
            .header(CONTENT_TYPE, "application/json")
            .timeout(Duration::from_secs(5))
            .body(serde_json::to_vec(req).unwrap())
            .send()
            .await?;
        let bytes = Self::check(resp).await?.bytes().await?;
        serde_json::from_slice(&bytes).map_err(|e| ClientError::Bad(e.to_string()))
    }

    pub async fn cancel(&self, host: &str, port: u16, generation: i64) -> Result<()> {
        let resp = self
            .http
            .post(url(host, port, "/sync/trust/v2/cancel"))
            .headers(self.headers(None, false))
            .header(HEADER_EXCHANGE_TIMESTAMP, generation.to_string())
            .header(CONTENT_TYPE, "application/json")
            .timeout(Duration::from_secs(3))
            .body("\"\"")
            .send()
            .await?;
        Self::check(resp).await?;
        Ok(())
    }

    /// `POST /sync/paste`. A 422 means the peer has receiving turned off,
    /// which CrossPaste treats as delivered.
    pub async fn send_paste(
        &self,
        host: &str,
        port: u16,
        target: &str,
        cipher: &MessageCipher,
        paste: &PasteData,
    ) -> Result<()> {
        let body = cipher.encrypt(&serde_json::to_vec(paste).unwrap());
        let resp = self
            .http
            .post(url(host, port, "/sync/paste"))
            .headers(self.headers(Some(target), true))
            .header(CONTENT_TYPE, "application/json")
            .timeout(Duration::from_secs(10))
            .body(body)
            .send()
            .await?;
        if resp.status().as_u16() == 422 {
            return Ok(());
        }
        Self::check(resp).await?;
        Ok(())
    }

    /// One ≤1 MiB chunk of the files of the peer's paste `remote_id`.
    pub async fn pull_file(
        &self,
        host: &str,
        port: u16,
        target: &str,
        cipher: &MessageCipher,
        remote_id: i64,
        chunk_index: i32,
    ) -> Result<Vec<u8>> {
        let req = PullFileRequest {
            id: remote_id,
            chunk_index,
        };
        let body = cipher.encrypt(&serde_json::to_vec(&req).unwrap());
        let resp = self
            .http
            .post(url(host, port, "/pull/file"))
            .headers(self.headers(Some(target), true))
            .header(CONTENT_TYPE, "application/json")
            .timeout(Duration::from_secs(50))
            .body(body)
            .send()
            .await?;
        let bytes = Self::check(resp).await?.bytes().await?;
        decrypt_records(cipher, &bytes)
    }
}

/// Splits `plain` into ≤256 KiB pieces, each encrypted and prefixed with its
/// big-endian length (`ServerEncryptPlugin` for streamed bodies).
pub fn encrypt_records(cipher: &MessageCipher, plain: &[u8]) -> Vec<u8> {
    const RECORD: usize = 256 * 1024;
    let mut out = Vec::with_capacity(plain.len() + plain.len() / RECORD * 36 + 36);
    for piece in plain.chunks(RECORD) {
        let ct = cipher.encrypt(piece);
        out.extend_from_slice(&(ct.len() as u32).to_be_bytes());
        out.extend_from_slice(&ct);
    }
    out
}

pub fn decrypt_records(cipher: &MessageCipher, data: &[u8]) -> Result<Vec<u8>> {
    const MAX_RECORD: usize = 1024 * 1024 + 32;
    let mut out = Vec::with_capacity(data.len());
    let mut rest = data;
    while !rest.is_empty() {
        if rest.len() < 4 {
            return Err(ClientError::Bad("truncated record length".into()));
        }
        let len = u32::from_be_bytes(rest[..4].try_into().unwrap()) as usize;
        rest = &rest[4..];
        if len == 0 || len > MAX_RECORD || len > rest.len() {
            return Err(ClientError::Bad(format!("bad record length {len}")));
        }
        let plain = cipher
            .decrypt(&rest[..len])
            .map_err(|_| ClientError::Peer {
                status: 200,
                code: error_code::DECRYPT_FAIL,
                message: "record decrypt failed".into(),
            })?;
        out.extend_from_slice(&plain);
        rest = &rest[len..];
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::KeyPair;

    #[test]
    fn records_round_trip_across_record_boundaries() {
        let a = KeyPair::generate();
        let b = KeyPair::generate();
        let enc = a.cipher_for(&b.crypt_public_der()).unwrap();
        let dec = b.cipher_for(&a.crypt_public_der()).unwrap();
        let plain: Vec<u8> = (0..(600 * 1024)).map(|i| (i % 251) as u8).collect();
        let wire = encrypt_records(&enc, &plain);
        assert_eq!(decrypt_records(&dec, &wire).unwrap(), plain);
        assert!(decrypt_records(&dec, &wire[..wire.len() - 1]).is_err());
    }
}
