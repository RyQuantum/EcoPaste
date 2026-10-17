//! Inbound HTTP API: the subset of CrossPaste's Ktor routes that a desktop
//! peer calls on a v2-paired desktop device.

// Handlers return axum `Response`s as their error type, like the Ktor routes respond directly.
#![allow(clippy::result_large_err)]

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use log::{debug, info, warn};
use rand::RngCore;

use crate::client::{
    encrypt_records, HEADER_APP_INSTANCE_ID, HEADER_EXCHANGE_TIMESTAMP, HEADER_SECURE,
    HEADER_TARGET_APP_INSTANCE_ID,
};
use crate::crypto::{self, MessageCipher};
use crate::model::{
    error_code, AuthenticatedControlRequest, ControlChallenge, FailResponse, KeyExchange,
    PasteData, PullFileRequest, SyncInfo, TrustConfirm, SYNC_API_VERSION,
};
use crate::node::{Event, Inner, PeerState, PendingExchange};

type S = State<Arc<Inner>>;

pub(crate) fn router(inner: Arc<Inner>) -> Router {
    Router::new()
        .route("/sync/telnet", get(telnet))
        .route("/sync/syncInfo", get(sync_info))
        .route("/sync/heartbeat", get(heartbeat))
        .route("/sync/heartbeat/syncInfo", post(heartbeat_with_info))
        .route("/sync/trust", post(trust_v1))
        .route("/sync/trust/v2/exchange", post(exchange))
        .route("/sync/trust/v2/confirm", post(confirm))
        .route("/sync/trust/v2/cancel", post(cancel))
        .route("/sync/showToken", get(ok_noop))
        .route("/sync/showPairingCode", get(show_pairing_code))
        .route("/sync/notifyExit", get(ok_noop))
        .route("/sync/notifyRemove", get(ok_noop))
        .route("/sync/control/challenge", get(control_challenge))
        .route("/sync/control/notifyExit", post(control_notify_exit))
        .route("/sync/control/notifyRemove", post(control_notify_remove))
        .route("/sync/paste", post(paste))
        .route("/pull/file", post(pull_file))
        .route("/pull/icon/{source}", get(pull_icon))
        .route("/pull/pasteBatch", get(pull_paste_batch))
        .route("/pull/paste", get(pull_paste))
        .with_state(inner)
}

// ---- response helpers ----------------------------------------------------

fn respond(status: StatusCode, content_type: &'static str, body: Vec<u8>) -> Response {
    let mut r = Response::new(Body::from(body));
    *r.status_mut() = status;
    r.headers_mut()
        .insert("content-type", HeaderValue::from_static(content_type));
    r
}

fn fail(code: i32, message: &str) -> Response {
    let body = serde_json::to_vec(&FailResponse {
        error_code: code,
        message: message.to_string(),
    })
    .unwrap();
    respond(
        StatusCode::from_u16(error_code::status(code)).unwrap(),
        "application/json",
        body,
    )
}

fn ok_json(value: &impl serde::Serialize, cipher: Option<&MessageCipher>) -> Response {
    let body = serde_json::to_vec(value).unwrap();
    let body = match cipher {
        Some(c) => c.encrypt(&body),
        None => body,
    };
    respond(StatusCode::OK, "application/json", body)
}

/// Ktor's `respond("")`: an empty text body, encrypted when the request was secure.
fn ok_empty(cipher: Option<&MessageCipher>) -> Response {
    let body = match cipher {
        Some(c) => c.encrypt(b""),
        None => Vec::new(),
    };
    respond(StatusCode::OK, "text/plain; charset=UTF-8", body)
}

fn header<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get(name).and_then(|v| v.to_str().ok())
}

fn is_secure(h: &HeaderMap) -> bool {
    h.contains_key(HEADER_SECURE)
}

/// `appInstanceId` header, or the CrossPaste error response for a missing one.
fn require_id(h: &HeaderMap) -> Result<String, Response> {
    header(h, HEADER_APP_INSTANCE_ID)
        .map(str::to_string)
        .ok_or_else(|| {
            fail(
                error_code::NOT_FOUND_APP_INSTANCE_ID,
                "NOT_FOUND_APP_INSTANCE_ID",
            )
        })
}

fn require_target(inner: &Inner, h: &HeaderMap) -> Result<(), Response> {
    if header(h, HEADER_TARGET_APP_INSTANCE_ID) != Some(inner.own_id.as_str()) {
        return Err(fail(
            error_code::NOT_MATCH_APP_INSTANCE_ID,
            "NOT_MATCH_APP_INSTANCE_ID",
        ));
    }
    Ok(())
}

/// Request body, decrypted with the sender's key when marked secure.
fn body_for(inner: &Inner, from: &str, h: &HeaderMap, body: &[u8]) -> Result<Vec<u8>, Response> {
    if !is_secure(h) {
        return Ok(body.to_vec());
    }
    let cipher = inner
        .cipher(from)
        .ok_or_else(|| fail(error_code::DECRYPT_FAIL, "DECRYPT_FAIL"))?;
    cipher
        .decrypt(body)
        .map_err(|_| fail(error_code::DECRYPT_FAIL, "DECRYPT_FAIL"))
}

/// The response cipher: requests marked secure get encrypted answers.
fn response_cipher(inner: &Inner, from: &str, h: &HeaderMap) -> Option<MessageCipher> {
    if is_secure(h) {
        inner.cipher(from)
    } else {
        None
    }
}

macro_rules! tri {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(resp) => return resp,
        }
    };
}

// ---- handlers --------------------------------------------------------------

async fn telnet(State(inner): S) -> Response {
    let mut r = ok_json(&SYNC_API_VERSION, None);
    r.headers_mut().insert(
        HEADER_APP_INSTANCE_ID,
        HeaderValue::from_str(&inner.own_id).unwrap(),
    );
    r
}

async fn sync_info(State(inner): S, ConnectInfo(addr): ConnectInfo<SocketAddr>) -> Response {
    ok_json(&inner.sync_info_for(&addr.ip().to_string()), None)
}

/// Common checks of `/sync/heartbeat*` and `/sync/control/challenge`.
fn validate_heartbeat(inner: &Inner, h: &HeaderMap) -> Result<String, Response> {
    let id = require_id(h)?;
    require_target(inner, h)?;
    if !inner.is_trusted(&id) {
        return Err(fail(error_code::DECRYPT_FAIL, "DECRYPT_FAIL"));
    }
    Ok(id)
}

async fn heartbeat(State(inner): S, h: HeaderMap) -> Response {
    tri!(validate_heartbeat(&inner, &h));
    ok_json(&SYNC_API_VERSION, None)
}

async fn heartbeat_with_info(
    State(inner): S,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    h: HeaderMap,
    body: Bytes,
) -> Response {
    let id = tri!(validate_heartbeat(&inner, &h));
    if header(&h, HEADER_SECURE) != Some("1") {
        return ok_json(&SYNC_API_VERSION, None);
    }
    let plain = tri!(body_for(&inner, &id, &h, &body));
    let Ok(mut info) = serde_json::from_slice::<SyncInfo>(&plain) else {
        return fail(error_code::UNKNOWN_ERROR, "UNKNOWN_ERROR");
    };
    info.app_info.app_instance_id = id.clone();
    inner.update_peer_info(info);
    inner.set_conn(&id, PeerState::Connected, Some(addr.ip().to_string()));
    ok_json(&SYNC_API_VERSION, response_cipher(&inner, &id, &h).as_ref())
}

async fn trust_v1(h: HeaderMap) -> Response {
    tri!(require_id(&h));
    // v1 bearer tokens (QR codes) are not supported; v2+ peers never use this route for us.
    fail(error_code::TOKEN_INVALID, "TOKEN_INVALID")
}

async fn exchange(State(inner): S, h: HeaderMap, body: Bytes) -> Response {
    let id = tri!(require_id(&h));
    let Ok(req) = serde_json::from_slice::<KeyExchange>(&body) else {
        return fail(error_code::EXCHANGE_FAIL, "EXCHANGE_FAIL");
    };
    let msg =
        crypto::key_exchange_message(&req.sign_public_key, &req.crypt_public_key, req.timestamp);
    if !crypto::verify(&req.sign_public_key, &msg, &req.signature)
        || !crypto::is_valid_public_der(&req.crypt_public_key)
    {
        warn!("crosspaste: exchange from {id} has a bad signature");
        return fail(error_code::SIGN_INVALID, "SIGN_INVALID");
    }
    let local_crypt = inner.keys.crypt_public_der();
    let local_sign = inner.keys.sign_public_der();
    let sas = crypto::compute_sas(&local_crypt, &req.crypt_public_key);
    let now = now_ms();

    // A repeat exchange (initiator warm-up, then the real one) supersedes the
    // earlier one; only announce a code the UI is not already showing.
    let announce = {
        let mut pending = inner.pending.lock().unwrap();
        let same_code = pending
            .get(&id)
            .map(|p| p.sas == sas && !p.rejected)
            .unwrap_or(false);
        pending.insert(
            id.clone(),
            PendingExchange {
                sign_public: req.sign_public_key.clone(),
                crypt_public: req.crypt_public_key.clone(),
                sas,
                generation: req.timestamp,
                at: Instant::now(),
                rejected: false,
            },
        );
        !same_code
    };
    if announce {
        info!(
            "crosspaste: pairing requested by {}",
            inner.device_name(&id)
        );
        inner.emit(Event::PairingRequested {
            peer_id: id.clone(),
            device_name: inner.device_name(&id),
            code: sas,
        });
    }

    let signature = inner.keys.sign(&crypto::key_exchange_message(
        &local_sign,
        &local_crypt,
        now,
    ));
    ok_json(
        &KeyExchange {
            sign_public_key: local_sign,
            crypt_public_key: local_crypt,
            timestamp: now,
            signature,
        },
        None,
    )
}

async fn confirm(State(inner): S, h: HeaderMap, body: Bytes) -> Response {
    let id = tri!(require_id(&h));
    let Ok(req) = serde_json::from_slice::<TrustConfirm>(&body) else {
        return fail(error_code::TRUST_FAIL, "TRUST_FAIL");
    };
    let pending = inner.pending.lock().unwrap().remove(&id);
    let Some(pending) = pending else {
        return fail(error_code::EXCHANGE_TIMEOUT, "EXCHANGE_TIMEOUT");
    };
    if pending.rejected || pending.at.elapsed() > std::time::Duration::from_secs(60) {
        inner.emit(Event::PairingCancelled { peer_id: id });
        return fail(error_code::EXCHANGE_TIMEOUT, "EXCHANGE_TIMEOUT");
    }
    if !crypto::verify(
        &pending.sign_public,
        &crypto::trust_confirm_message(req.timestamp),
        &req.signature,
    ) {
        return fail(error_code::SIGN_INVALID, "SIGN_INVALID");
    }
    inner.trust(&id, &pending.crypt_public, None);
    let now = now_ms();
    let signature = inner.keys.sign(&crypto::trust_confirm_message(now));
    let name = inner.device_name(&id);
    info!("crosspaste: paired with {name}");
    inner.emit(Event::Paired {
        peer_id: id.clone(),
        device_name: name,
    });
    inner.emit(Event::DevicesChanged);
    ok_json(
        &TrustConfirm {
            timestamp: now,
            signature,
        },
        None,
    )
}

async fn cancel(State(inner): S, h: HeaderMap) -> Response {
    let id = tri!(require_id(&h));
    let generation = header(&h, HEADER_EXCHANGE_TIMESTAMP).and_then(|v| v.parse::<i64>().ok());
    let removed = {
        let mut pending = inner.pending.lock().unwrap();
        let matches = match (pending.get(&id), generation) {
            (Some(_), None) => true,
            (Some(p), Some(g)) => p.generation == g,
            (None, _) => false,
        };
        matches && pending.remove(&id).is_some()
    };
    if removed {
        inner.emit(Event::PairingCancelled { peer_id: id });
    }
    ok_empty(None)
}

async fn ok_noop(h: HeaderMap) -> Response {
    debug!(
        "crosspaste: ignored legacy call from {:?}",
        header(&h, HEADER_APP_INSTANCE_ID)
    );
    ok_empty(None)
}

async fn show_pairing_code() -> Response {
    // Pairing v3 only; we advertise pairingVersion 2.
    fail(
        error_code::REMOTE_SHOW_PAIRING_CODE_DISABLED,
        "REMOTE_SHOW_PAIRING_CODE_DISABLED",
    )
}

async fn control_challenge(State(inner): S, h: HeaderMap) -> Response {
    let id = tri!(validate_heartbeat(&inner, &h));
    let mut cid = vec![0u8; 16];
    let mut nonce = vec![0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut cid);
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    inner
        .challenges
        .lock()
        .unwrap()
        .insert(id, (cid.clone(), nonce.clone(), Instant::now()));
    ok_json(&ControlChallenge { id: cid, nonce }, None)
}

fn authenticated_control(
    inner: &Inner,
    h: &HeaderMap,
    body: &[u8],
    operation: &str,
) -> Result<String, Response> {
    let id = require_id(h)?;
    if header(h, HEADER_SECURE) != Some("1") {
        return Err(fail(error_code::DECRYPT_FAIL, "DECRYPT_FAIL"));
    }
    validate_heartbeat(inner, h)?;
    let plain = body_for(inner, &id, h, body)?;
    let req: AuthenticatedControlRequest = serde_json::from_slice(&plain)
        .map_err(|_| fail(error_code::DECRYPT_FAIL, "DECRYPT_FAIL"))?;
    let challenge = inner.challenges.lock().unwrap().remove(&id);
    let valid = matches!(challenge, Some((cid, nonce, at))
        if cid == req.challenge_id && nonce == req.challenge_nonce && at.elapsed().as_secs() < 30)
        && req.target_app_instance_id == inner.own_id
        && req.operation == operation;
    if !valid {
        return Err(fail(error_code::DECRYPT_FAIL, "DECRYPT_FAIL"));
    }
    Ok(id)
}

async fn control_notify_exit(State(inner): S, h: HeaderMap, body: Bytes) -> Response {
    let id = tri!(authenticated_control(&inner, &h, &body, "notify_exit"));
    info!("crosspaste: {} is exiting", inner.device_name(&id));
    inner.set_conn(&id, PeerState::Disconnected, None);
    ok_empty(response_cipher(&inner, &id, &h).as_ref())
}

async fn control_notify_remove(State(inner): S, h: HeaderMap, body: Bytes) -> Response {
    let id = tri!(authenticated_control(&inner, &h, &body, "notify_remove"));
    let cipher = response_cipher(&inner, &id, &h);
    info!("crosspaste: {} removed us", inner.device_name(&id));
    inner.untrust(&id);
    ok_empty(cipher.as_ref())
}

async fn paste(State(inner): S, h: HeaderMap, body: Bytes) -> Response {
    let id = tri!(require_id(&h));
    tri!(require_target(&inner, &h));
    if !inner.is_trusted(&id) {
        return fail(
            error_code::NOT_FOUND_APP_INSTANCE_ID,
            "NOT_FOUND_APP_INSTANCE_ID",
        );
    }
    if !inner.receive_enabled.load(Ordering::Relaxed) {
        return fail(
            error_code::SYNC_NOT_ALLOW_RECEIVE_BY_APP,
            "SYNC_NOT_ALLOW_RECEIVE_BY_APP",
        );
    }
    if header(&h, "X-Sync-Mode") == Some("push") {
        // Push-mode uploads (mobile share sheet) are not implemented.
        return fail(error_code::PUSH_SESSION_REJECTED, "PUSH_SESSION_REJECTED");
    }
    let plain = tri!(body_for(&inner, &id, &h, &body));
    let mut data: PasteData = match serde_json::from_slice(&plain) {
        Ok(d) => d,
        Err(e) => {
            warn!("crosspaste: unreadable paste from {id}: {e}");
            return fail(error_code::UNKNOWN_ERROR, "UNKNOWN_ERROR");
        }
    };
    data.app_instance_id = id.clone();
    debug!(
        "crosspaste: paste {} type {} from {id}",
        data.id, data.paste_type
    );
    inner.accept_paste(id.clone(), data);
    ok_empty(response_cipher(&inner, &id, &h).as_ref())
}

async fn pull_file(State(inner): S, h: HeaderMap, body: Bytes) -> Response {
    let id = tri!(require_id(&h));
    tri!(require_target(&inner, &h));
    let plain = tri!(body_for(&inner, &id, &h, &body));
    let Ok(req) = serde_json::from_slice::<PullFileRequest>(&plain) else {
        return fail(error_code::UNKNOWN_ERROR, "UNKNOWN_ERROR");
    };
    if !inner.is_trusted(&id) {
        return fail(
            error_code::NOT_FOUND_APP_INSTANCE_ID,
            "NOT_FOUND_APP_INSTANCE_ID",
        );
    }
    let chunk = match inner.read_outbox_chunk(req.id, req.chunk_index) {
        Ok(c) => c,
        Err(code) => return fail(code, "file chunk unavailable"),
    };
    debug!(
        "crosspaste: serving chunk {} of {} ({} bytes) to {id}",
        req.chunk_index,
        req.id,
        chunk.len()
    );
    let body = match response_cipher(&inner, &id, &h) {
        Some(c) => encrypt_records(&c, &chunk),
        None => chunk,
    };
    respond(StatusCode::OK, "application/octet-stream", body)
}

async fn pull_icon() -> Response {
    fail(error_code::NOT_FOUND_ICON, "NOT_FOUND_ICON")
}

async fn pull_paste_batch(State(inner): S, h: HeaderMap) -> Response {
    let id = tri!(require_id(&h));
    tri!(require_target(&inner, &h));
    if !inner.is_trusted(&id) {
        return fail(
            error_code::NOT_FOUND_APP_INSTANCE_ID,
            "NOT_FOUND_APP_INSTANCE_ID",
        );
    }
    // No history catch-up: peers only get what is copied while connected.
    ok_json(
        &Vec::<PasteData>::new(),
        response_cipher(&inner, &id, &h).as_ref(),
    )
}

async fn pull_paste(State(inner): S, h: HeaderMap) -> Response {
    tri!(require_id(&h));
    tri!(require_target(&inner, &h));
    fail(
        error_code::SYNC_PASTE_NOT_FOUND_DATA,
        "SYNC_PASTE_NOT_FOUND_DATA",
    )
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
