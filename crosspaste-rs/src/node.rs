//! The sync engine: owns identity, peer state, the HTTP server, mDNS and the
//! connect/heartbeat loop. Applications talk to it through [`Node`] and get
//! told about incoming content through an [`Event`] callback.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use log::{debug, info, warn};

use crate::client::Client;
use crate::crypto::{self, MessageCipher};
use crate::discovery::{self, Discovered, Discovery};
use crate::model::{
    error_code, AppInfo, EndpointInfo, HostInfo, KeyExchange, PasteData, Platform, RemoteContent,
    RemoteFile, SyncInfo, TrustConfirm, PAIRING_VERSION, SYNC_API_VERSION,
};
use crate::store::{Store, TrustedPeer};

/// CrossPaste's file transfer chunk size.
pub const CHUNK_SIZE: u64 = 1024 * 1024;
const PENDING_EXCHANGE_TTL: Duration = Duration::from_secs(60);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const OUTBOX_KEEP: usize = 10;
const MAX_RECEIVE_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Config {
    /// Where identity, outbox and inbox live.
    pub data_dir: PathBuf,
    pub device_name: String,
    pub user_name: String,
    /// Shown as the app version in the peer's device list.
    pub app_version: String,
    pub app_revision: String,
    /// Preferred TCP port; falls back to a random one when taken. 0 = random.
    pub port: u16,
    pub platform: Platform,
    /// Address to listen on; `None` = all interfaces.
    pub bind_ip: Option<std::net::IpAddr>,
    /// Addresses to advertise instead of the detected LAN addresses.
    pub advertise_hosts: Option<Vec<HostInfo>>,
    /// Advertise and browse over mDNS. Off only for loopback tests.
    pub mdns: bool,
}

impl Config {
    pub fn platform_windows() -> Platform {
        Platform {
            name: "Windows".into(),
            arch: "amd64".into(),
            bit_mode: 64,
            version: "11".into(),
        }
    }

    pub fn platform_current() -> Platform {
        let name = match std::env::consts::OS {
            "windows" => "Windows",
            "macos" => "Macos",
            _ => "Linux",
        };
        let arch = match std::env::consts::ARCH {
            "x86_64" => "amd64",
            other => other,
        };
        Platform {
            name: name.into(),
            arch: arch.into(),
            bit_mode: 64,
            version: if name == "Windows" {
                "11".into()
            } else {
                "Unknown".into()
            },
        }
    }
}

#[derive(Clone, Debug)]
pub enum Event {
    /// Nearby or paired device list changed.
    DevicesChanged,
    /// Another device started pairing with us; show `code` until it finishes.
    PairingRequested {
        peer_id: String,
        device_name: String,
        code: u32,
    },
    /// Pairing finished; we now trust the peer.
    Paired {
        peer_id: String,
        device_name: String,
    },
    /// The peer abandoned pairing (or it expired).
    PairingCancelled {
        peer_id: String,
    },
    /// The peer removed us, or we removed it.
    Unpaired {
        peer_id: String,
    },
    RemoteText {
        peer_id: String,
        text: String,
    },
    /// Downloaded and hash-verified image files, in the order the peer listed them.
    RemoteImages {
        peer_id: String,
        paths: Vec<PathBuf>,
    },
}

pub type EventSink = Arc<dyn Fn(Event) + Send + Sync>;

#[derive(Clone, Debug, PartialEq)]
pub enum PeerState {
    Disconnected,
    Connected,
    /// Reachable but reports a different `SyncApi.VERSION`.
    Incompatible(i32),
    /// Reachable but cannot decrypt with our key (it forgot us).
    KeyMismatch,
}

#[derive(Clone, Debug)]
pub struct DeviceInfo {
    pub app_instance_id: String,
    pub device_name: String,
    pub platform: String,
    pub app_version: String,
    pub paired: bool,
    pub state: PeerState,
    pub host: Option<String>,
}

pub(crate) struct PendingExchange {
    pub sign_public: Vec<u8>,
    pub crypt_public: Vec<u8>,
    pub sas: u32,
    pub generation: i64,
    pub at: Instant,
    pub rejected: bool,
}

#[derive(Clone)]
pub(crate) struct Conn {
    pub state: PeerState,
    pub host: Option<String>,
    pub last_ok: Option<Instant>,
    /// Whether this session already sent our SyncInfo to the peer.
    pub advertised: bool,
}

impl Default for Conn {
    fn default() -> Self {
        Self {
            state: PeerState::Disconnected,
            host: None,
            last_ok: None,
            advertised: false,
        }
    }
}

/// Issued control challenge: (id, nonce, issued at).
pub(crate) type Challenge = (Vec<u8>, Vec<u8>, Instant);

pub(crate) struct OutboxEntry {
    pub id: i64,
    pub files: Vec<(PathBuf, u64)>,
}

pub(crate) struct Inner {
    pub cfg: Config,
    pub own_id: String,
    pub keys: crypto::KeyPair,
    pub store: Mutex<Store>,
    pub port: AtomicU16,
    pub client: Client,
    pub events: EventSink,
    pub nearby: Mutex<HashMap<String, SyncInfo>>,
    pub conns: Mutex<HashMap<String, Conn>>,
    pub pending: Mutex<HashMap<String, PendingExchange>>,
    pub challenges: Mutex<HashMap<String, Challenge>>,
    pub ciphers: Mutex<HashMap<String, MessageCipher>>,
    pub outbox: Mutex<VecDeque<OutboxEntry>>,
    pub next_id: AtomicI64,
    pub local_hosts: Mutex<Vec<HostInfo>>,
    pub receive_enabled: std::sync::atomic::AtomicBool,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl Inner {
    pub fn emit(&self, e: Event) {
        (self.events)(e)
    }

    pub fn sync_info(&self) -> SyncInfo {
        SyncInfo {
            app_info: AppInfo {
                app_instance_id: self.own_id.clone(),
                app_version: self.cfg.app_version.clone(),
                app_revision: self.cfg.app_revision.clone(),
                user_name: self.cfg.user_name.clone(),
                pairing_version: Some(PAIRING_VERSION),
            },
            endpoint_info: EndpointInfo {
                device_id: self.own_id.clone(),
                device_name: self.cfg.device_name.clone(),
                platform: self.cfg.platform.clone(),
                host_info_list: self.local_hosts.lock().unwrap().clone(),
                port: self.port.load(Ordering::Relaxed),
            },
        }
    }

    /// Our SyncInfo limited to addresses on `peer_host`'s subnet (what CrossPaste advertises).
    pub fn sync_info_for(&self, peer_host: &str) -> SyncInfo {
        let mut info = self.sync_info();
        let matched: Vec<HostInfo> = info
            .endpoint_info
            .host_info_list
            .iter()
            .filter(|h| discovery::same_subnet(&h.host_address, peer_host, h.network_prefix_length))
            .cloned()
            .collect();
        if !matched.is_empty() {
            info.endpoint_info.host_info_list = matched;
        }
        info
    }

    pub fn is_trusted(&self, id: &str) -> bool {
        self.store.lock().unwrap().peers.contains_key(id)
    }

    pub fn cipher(&self, id: &str) -> Option<MessageCipher> {
        if let Some(c) = self.ciphers.lock().unwrap().get(id) {
            return Some(c.clone());
        }
        let key = self.store.lock().unwrap().peer_crypt_key(id)?;
        let cipher = self.keys.cipher_for(&key).ok()?;
        self.ciphers
            .lock()
            .unwrap()
            .insert(id.to_string(), cipher.clone());
        Some(cipher)
    }

    pub fn trust(&self, id: &str, crypt_public: &[u8], sync_info: Option<SyncInfo>) {
        let info = sync_info.or_else(|| self.nearby.lock().unwrap().get(id).cloned());
        {
            let mut store = self.store.lock().unwrap();
            store.peers.insert(
                id.to_string(),
                TrustedPeer {
                    crypt_public_key: B64.encode(crypt_public),
                    sync_info: info,
                    paired_at: now_ms(),
                },
            );
            if let Err(e) = store.save() {
                warn!("crosspaste: saving store failed: {e:#}");
            }
        }
        self.ciphers.lock().unwrap().remove(id);
        self.conns
            .lock()
            .unwrap()
            .insert(id.to_string(), Conn::default());
    }

    pub fn untrust(&self, id: &str) -> bool {
        let removed = {
            let mut store = self.store.lock().unwrap();
            let removed = store.peers.remove(id).is_some();
            if removed {
                if let Err(e) = store.save() {
                    warn!("crosspaste: saving store failed: {e:#}");
                }
            }
            removed
        };
        self.ciphers.lock().unwrap().remove(id);
        self.conns.lock().unwrap().remove(id);
        if removed {
            self.emit(Event::Unpaired {
                peer_id: id.to_string(),
            });
            self.emit(Event::DevicesChanged);
        }
        removed
    }

    pub fn device_name(&self, id: &str) -> String {
        if let Some(info) = self.nearby.lock().unwrap().get(id) {
            return info.endpoint_info.device_name.clone();
        }
        self.store
            .lock()
            .unwrap()
            .peers
            .get(id)
            .and_then(|p| p.sync_info.as_ref())
            .map(|s| s.endpoint_info.device_name.clone())
            .unwrap_or_else(|| id.to_string())
    }

    /// Records a peer's SyncInfo learned from mDNS or an authenticated heartbeat.
    pub fn update_peer_info(&self, info: SyncInfo) {
        let id = info.app_info.app_instance_id.clone();
        let mut store = self.store.lock().unwrap();
        if let Some(peer) = store.peers.get_mut(&id) {
            if peer.sync_info.as_ref() != Some(&info) {
                peer.sync_info = Some(info);
                if let Err(e) = store.save() {
                    warn!("crosspaste: saving store failed: {e:#}");
                }
            }
        }
    }

    pub fn set_conn(&self, id: &str, state: PeerState, host: Option<String>) {
        let changed = {
            let mut conns = self.conns.lock().unwrap();
            let c = conns.entry(id.to_string()).or_default();
            let changed = c.state != state || (host.is_some() && c.host != host);
            if state == PeerState::Connected {
                c.last_ok = Some(Instant::now());
            } else {
                c.advertised = false;
            }
            c.state = state.clone();
            if host.is_some() {
                c.host = host;
            }
            changed
        };
        if changed {
            info!("crosspaste: {} -> {:?}", self.device_name(id), state);
            self.emit(Event::DevicesChanged);
        }
    }

    /// (host, port) candidates for a trusted peer, the last good host first.
    fn candidates(&self, id: &str) -> (Vec<String>, Option<u16>) {
        let info = self.nearby.lock().unwrap().get(id).cloned().or_else(|| {
            self.store
                .lock()
                .unwrap()
                .peers
                .get(id)
                .and_then(|p| p.sync_info.clone())
        });
        let mut hosts = Vec::new();
        if let Some(h) = self
            .conns
            .lock()
            .unwrap()
            .get(id)
            .and_then(|c| c.host.clone())
        {
            hosts.push(h);
        }
        let port = info.as_ref().map(|i| i.endpoint_info.port);
        if let Some(info) = info {
            let locals = self.local_hosts.lock().unwrap().clone();
            let mut matched = Vec::new();
            let mut others = Vec::new();
            for h in info.endpoint_info.host_info_list {
                if hosts.contains(&h.host_address) {
                    continue;
                }
                if locals.iter().any(|l| {
                    discovery::same_subnet(
                        &l.host_address,
                        &h.host_address,
                        l.network_prefix_length,
                    )
                }) {
                    matched.push(h.host_address);
                } else {
                    others.push(h.host_address);
                }
            }
            hosts.extend(matched);
            hosts.extend(others);
        }
        (hosts, port)
    }

    pub fn peer_port(&self, id: &str) -> Option<u16> {
        self.candidates(id).1
    }

    /// Connects to (or heartbeats) one trusted peer.
    async fn resolve(self: &Arc<Self>, id: String) {
        let conn = self
            .conns
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .unwrap_or_default();
        if conn.state == PeerState::Connected
            && conn
                .last_ok
                .map(|t| t.elapsed() < HEARTBEAT_INTERVAL)
                .unwrap_or(false)
        {
            return;
        }
        let Some(cipher) = self.cipher(&id) else {
            return;
        };
        let (hosts, port) = self.candidates(&id);
        let Some(port) = port else { return };

        for host in hosts {
            let telnet = match self
                .client
                .telnet(&host, port, Duration::from_millis(1500))
                .await
            {
                Ok(t) => t,
                Err(e) => {
                    debug!("crosspaste: telnet {host}:{port} failed: {e}");
                    continue;
                }
            };
            if telnet
                .app_instance_id
                .as_deref()
                .is_some_and(|got| got != id)
            {
                debug!("crosspaste: {host} answers as another device");
                continue;
            }
            if telnet.version != SYNC_API_VERSION {
                self.set_conn(&id, PeerState::Incompatible(telnet.version), Some(host));
                return;
            }
            let result = if conn.advertised && conn.state == PeerState::Connected {
                self.client.heartbeat(&host, port, &id).await
            } else {
                let info = self.sync_info_for(&host);
                self.client
                    .heartbeat_with_info(&host, port, &id, &cipher, &info)
                    .await
            };
            match result {
                Ok(v) if v == SYNC_API_VERSION => {
                    self.set_conn(&id, PeerState::Connected, Some(host));
                    if let Some(c) = self.conns.lock().unwrap().get_mut(&id) {
                        c.advertised = true;
                    }
                    return;
                }
                Ok(v) => {
                    self.set_conn(&id, PeerState::Incompatible(v), Some(host));
                    return;
                }
                Err(e) if e.peer_code() == Some(error_code::DECRYPT_FAIL) => {
                    warn!(
                        "crosspaste: {} cannot decrypt our messages (it may have unpaired us)",
                        self.device_name(&id)
                    );
                    self.set_conn(&id, PeerState::KeyMismatch, Some(host));
                    return;
                }
                Err(e) => debug!("crosspaste: heartbeat {host}:{port} failed: {e}"),
            }
        }
        self.set_conn(&id, PeerState::Disconnected, None);
    }

    fn connected_peers(&self) -> Vec<(String, String, u16)> {
        let ids: Vec<String> = self.store.lock().unwrap().peers.keys().cloned().collect();
        ids.into_iter()
            .filter_map(|id| {
                let c = self.conns.lock().unwrap().get(&id).cloned()?;
                if c.state != PeerState::Connected {
                    return None;
                }
                let host = c.host?;
                let port = self.peer_port(&id)?;
                Some((id, host, port))
            })
            .collect()
    }

    pub fn next_paste_id(&self) -> i64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    async fn broadcast(self: &Arc<Self>, paste: PasteData) {
        let peers = self.connected_peers();
        if peers.is_empty() {
            debug!("crosspaste: no connected peers for paste {}", paste.id);
        }
        let mut tasks = Vec::new();
        for (id, host, port) in peers {
            let inner = self.clone();
            let paste = paste.clone();
            tasks.push(tokio::spawn(async move {
                let Some(cipher) = inner.cipher(&id) else {
                    return;
                };
                for attempt in 0..2 {
                    match inner
                        .client
                        .send_paste(&host, port, &id, &cipher, &paste)
                        .await
                    {
                        Ok(()) => {
                            info!(
                                "crosspaste: sent paste {} to {}",
                                paste.id,
                                inner.device_name(&id)
                            );
                            return;
                        }
                        Err(e) => {
                            warn!(
                                "crosspaste: send to {} failed (attempt {}): {e}",
                                inner.device_name(&id),
                                attempt + 1
                            );
                            tokio::time::sleep(Duration::from_secs(1)).await;
                        }
                    }
                }
                inner.set_conn(&id, PeerState::Disconnected, None);
            }));
        }
        for t in tasks {
            let _ = t.await;
        }
    }

    pub fn outbox_dir(&self) -> PathBuf {
        self.cfg.data_dir.join("outbox")
    }

    pub fn inbox_dir(&self) -> PathBuf {
        self.cfg.data_dir.join("inbox")
    }

    /// Bytes `[offset, offset+len)` of the concatenated files of an outbox entry.
    pub fn read_outbox_chunk(&self, id: i64, chunk_index: i32) -> Result<Vec<u8>, i32> {
        let files = {
            let outbox = self.outbox.lock().unwrap();
            let entry = outbox
                .iter()
                .find(|e| e.id == id)
                .ok_or(error_code::NOT_FOUND_FILES_INDEX)?;
            entry.files.clone()
        };
        let total: u64 = files.iter().map(|(_, s)| *s).sum();
        let chunks = total.div_ceil(CHUNK_SIZE).max(1);
        if chunk_index < 0 || chunk_index as u64 >= chunks {
            return Err(error_code::OUT_RANGE_CHUNK_INDEX);
        }
        let start = chunk_index as u64 * CHUNK_SIZE;
        let end = (start + CHUNK_SIZE).min(total);
        let mut out = Vec::with_capacity((end - start) as usize);
        let mut file_start = 0u64;
        for (path, size) in files {
            let file_end = file_start + size;
            if file_end > start && file_start < end {
                let from = start.max(file_start) - file_start;
                let to = end.min(file_end) - file_start;
                let data = std::fs::read(&path).map_err(|_| error_code::NOT_FOUND_FILES_INDEX)?;
                if data.len() as u64 != size {
                    return Err(error_code::NOT_FOUND_FILES_INDEX);
                }
                out.extend_from_slice(&data[from as usize..to as usize]);
            }
            file_start = file_end;
        }
        Ok(out)
    }

    /// Handles an accepted remote paste in the background.
    pub fn accept_paste(self: &Arc<Self>, from: String, paste: PasteData) {
        let inner = self.clone();
        tokio::spawn(async move {
            match paste.content() {
                RemoteContent::Text(text) => {
                    info!(
                        "crosspaste: text from {} ({} bytes)",
                        inner.device_name(&from),
                        text.len()
                    );
                    inner.emit(Event::RemoteText {
                        peer_id: from,
                        text,
                    });
                }
                RemoteContent::Images(files) => {
                    match inner.pull_images(&from, paste.id, &files).await {
                        Ok(paths) => {
                            info!(
                                "crosspaste: {} image(s) from {}",
                                paths.len(),
                                inner.device_name(&from)
                            );
                            inner.emit(Event::RemoteImages {
                                peer_id: from,
                                paths,
                            });
                        }
                        Err(e) => warn!(
                            "crosspaste: pulling images from {} failed: {e:#}",
                            inner.device_name(&from)
                        ),
                    }
                }
                RemoteContent::Unsupported(t) => {
                    debug!("crosspaste: ignoring paste type {t} from {from}")
                }
            }
        });
    }

    async fn pull_images(
        &self,
        from: &str,
        remote_id: i64,
        files: &[RemoteFile],
    ) -> anyhow::Result<Vec<PathBuf>> {
        let total: u64 = files.iter().map(|f| f.size).sum();
        anyhow::ensure!(
            total > 0 && total <= MAX_RECEIVE_BYTES,
            "refusing {total} bytes"
        );
        for f in files {
            anyhow::ensure!(is_safe_file_name(&f.name), "unsafe file name {:?}", f.name);
        }
        let cipher = self
            .cipher(from)
            .ok_or_else(|| anyhow::anyhow!("not paired"))?;
        let host = self
            .conns
            .lock()
            .unwrap()
            .get(from)
            .and_then(|c| c.host.clone())
            .ok_or_else(|| anyhow::anyhow!("no address"))?;
        let port = self
            .peer_port(from)
            .ok_or_else(|| anyhow::anyhow!("no port"))?;

        let chunks = total.div_ceil(CHUNK_SIZE);
        let mut data = Vec::with_capacity(total as usize);
        for i in 0..chunks {
            let chunk = self
                .client
                .pull_file(&host, port, from, &cipher, remote_id, i as i32)
                .await?;
            let expected = (total - i * CHUNK_SIZE).min(CHUNK_SIZE) as usize;
            anyhow::ensure!(
                chunk.len() == expected,
                "chunk {i}: got {} bytes, want {expected}",
                chunk.len()
            );
            data.extend_from_slice(&chunk);
        }

        let dir = self.inbox_dir();
        std::fs::create_dir_all(&dir)?;
        prune_dir(&dir, 20);
        let mut paths = Vec::new();
        let mut offset = 0usize;
        for f in files {
            let bytes = &data[offset..offset + f.size as usize];
            offset += f.size as usize;
            anyhow::ensure!(
                crate::hash::hash(bytes) == f.hash,
                "hash mismatch for {}",
                f.name
            );
            let path = dir.join(format!("{}-{}", now_ms(), f.name));
            std::fs::write(&path, bytes)?;
            paths.push(path);
        }
        Ok(paths)
    }
}

fn is_safe_file_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', ':', '\0'])
}

/// Keeps the newest `keep` files of `dir`.
fn prune_dir(dir: &Path, keep: usize) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    for (_, p) in files.into_iter().skip(keep) {
        let _ = std::fs::remove_file(p);
    }
}

/// Handle to a running sync node. Cheap to clone.
#[derive(Clone)]
pub struct Node {
    inner: Arc<Inner>,
    shutdown: Arc<tokio::sync::Notify>,
    discovery: Arc<Mutex<Option<Discovery>>>,
}

impl Node {
    /// Starts server, mDNS and the connect loop on the current tokio runtime.
    pub async fn start(cfg: Config, events: EventSink) -> anyhow::Result<Node> {
        let store = Store::open(&cfg.data_dir)?;
        let own_id = store.app_instance_id.clone();
        let keys = store.keys.clone();
        let peer_ids: Vec<String> = store.peers.keys().cloned().collect();

        let bind_ip = cfg.bind_ip.unwrap_or(std::net::IpAddr::from([0, 0, 0, 0]));
        let listener = match tokio::net::TcpListener::bind(SocketAddr::new(bind_ip, cfg.port)).await
        {
            Ok(l) => l,
            Err(e) => {
                warn!(
                    "crosspaste: port {} unavailable ({e}), using a random port",
                    cfg.port
                );
                tokio::net::TcpListener::bind(SocketAddr::new(bind_ip, 0)).await?
            }
        };
        let port = listener.local_addr()?.port();

        let inner = Arc::new(Inner {
            client: Client::new(own_id.clone()),
            own_id: own_id.clone(),
            keys,
            store: Mutex::new(store),
            port: AtomicU16::new(port),
            events,
            nearby: Mutex::new(HashMap::new()),
            conns: Mutex::new(
                peer_ids
                    .into_iter()
                    .map(|id| (id, Conn::default()))
                    .collect(),
            ),
            pending: Mutex::new(HashMap::new()),
            challenges: Mutex::new(HashMap::new()),
            ciphers: Mutex::new(HashMap::new()),
            outbox: Mutex::new(VecDeque::new()),
            next_id: AtomicI64::new(now_ms()),
            local_hosts: Mutex::new(
                cfg.advertise_hosts
                    .clone()
                    .unwrap_or_else(discovery::local_hosts),
            ),
            receive_enabled: std::sync::atomic::AtomicBool::new(true),
            cfg,
        });
        let _ = std::fs::remove_dir_all(inner.outbox_dir());
        info!("crosspaste: {} listening on port {port}", own_id);

        let shutdown = Arc::new(tokio::sync::Notify::new());
        {
            let app = crate::server::router(inner.clone());
            let stop = shutdown.clone();
            tokio::spawn(async move {
                let serve = axum::serve(
                    listener,
                    app.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .with_graceful_shutdown(async move { stop.notified().await });
                if let Err(e) = serve.await {
                    warn!("crosspaste: server stopped: {e}");
                }
            });
        }

        let discovery = Arc::new(Mutex::new(None));
        if inner.cfg.mdns {
            let mut disc = Discovery::new()?;
            disc.advertise(&inner.sync_info())?;
            let browse_inner = inner.clone();
            let handle = tokio::runtime::Handle::current();
            disc.browse(own_id.clone(), move |event| {
                let inner = browse_inner.clone();
                handle.spawn(async move { on_discovered(&inner, event).await });
            })?;
            *discovery.lock().unwrap() = Some(disc);
        }

        {
            let inner = inner.clone();
            let stop = shutdown.clone();
            let discovery = discovery.clone();
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(Duration::from_secs(3));
                let mut rounds = 0u64;
                loop {
                    tokio::select! {
                        _ = stop.notified() => break,
                        _ = tick.tick() => {}
                    }
                    rounds += 1;
                    if rounds.is_multiple_of(10) {
                        refresh_local_hosts(&inner, &discovery);
                    }
                    expire_pending(&inner);
                    let ids: Vec<String> =
                        inner.store.lock().unwrap().peers.keys().cloned().collect();
                    let tasks: Vec<_> = ids
                        .into_iter()
                        .map(|id| {
                            let inner = inner.clone();
                            tokio::spawn(async move { inner.resolve(id).await })
                        })
                        .collect();
                    for t in tasks {
                        let _ = t.await;
                    }
                }
            });
        }

        Ok(Node {
            inner,
            shutdown,
            discovery,
        })
    }

    pub fn app_instance_id(&self) -> &str {
        &self.inner.own_id
    }

    pub fn port(&self) -> u16 {
        self.inner.port.load(Ordering::Relaxed)
    }

    pub fn set_receive_enabled(&self, enabled: bool) {
        self.inner.receive_enabled.store(enabled, Ordering::Relaxed);
    }

    /// Nearby and paired devices.
    pub fn devices(&self) -> Vec<DeviceInfo> {
        let nearby = self.inner.nearby.lock().unwrap().clone();
        let store_peers: Vec<(String, Option<SyncInfo>)> = self
            .inner
            .store
            .lock()
            .unwrap()
            .peers
            .iter()
            .map(|(k, v)| (k.clone(), v.sync_info.clone()))
            .collect();
        let conns = self.inner.conns.lock().unwrap().clone();
        let mut out = Vec::new();
        for (id, info) in &store_peers {
            let info = nearby.get(id).or(info.as_ref());
            let conn = conns.get(id).cloned().unwrap_or_default();
            out.push(DeviceInfo {
                app_instance_id: id.clone(),
                device_name: info
                    .map(|i| i.endpoint_info.device_name.clone())
                    .unwrap_or_else(|| id.clone()),
                platform: info
                    .map(|i| i.endpoint_info.platform.name.clone())
                    .unwrap_or_default(),
                app_version: info
                    .map(|i| i.app_info.app_version.clone())
                    .unwrap_or_default(),
                paired: true,
                state: conn.state,
                host: conn.host,
            });
        }
        for (id, info) in nearby {
            if store_peers.iter().any(|(k, _)| *k == id) {
                continue;
            }
            out.push(DeviceInfo {
                app_instance_id: id.clone(),
                device_name: info.endpoint_info.device_name.clone(),
                platform: info.endpoint_info.platform.name.clone(),
                app_version: info.app_info.app_version.clone(),
                paired: false,
                state: PeerState::Disconnected,
                host: info
                    .endpoint_info
                    .host_info_list
                    .first()
                    .map(|h| h.host_address.clone()),
            });
        }
        out
    }

    /// Whether any paired device is currently connected (lets callers skip
    /// expensive preparation such as image transcoding).
    pub fn has_connected_peers(&self) -> bool {
        !self.inner.connected_peers().is_empty()
    }

    /// Sends text to every connected peer.
    pub async fn send_text(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        let paste = PasteData::text(self.inner.next_paste_id(), &self.inner.own_id, text);
        self.inner.broadcast(paste).await;
    }

    /// Sends one already-encoded image (PNG or JPEG bytes). `ext` is "png" or "jpg".
    pub async fn send_image(&self, bytes: Vec<u8>, ext: &str) -> anyhow::Result<()> {
        if self.inner.connected_peers().is_empty() {
            return Ok(());
        }
        let id = self.inner.next_paste_id();
        let dir = self.inner.outbox_dir();
        std::fs::create_dir_all(&dir)?;
        let name = format!("EcoPaste-{id}.{ext}");
        let path = dir.join(&name);
        std::fs::write(&path, &bytes)?;
        {
            let mut outbox = self.inner.outbox.lock().unwrap();
            outbox.push_back(OutboxEntry {
                id,
                files: vec![(path, bytes.len() as u64)],
            });
            while outbox.len() > OUTBOX_KEEP {
                if let Some(old) = outbox.pop_front() {
                    for (p, _) in old.files {
                        let _ = std::fs::remove_file(p);
                    }
                }
            }
        }
        let paste = PasteData::image(id, &self.inner.own_id, &name, &bytes);
        self.inner.broadcast(paste).await;
        Ok(())
    }

    /// Adds a device by address without mDNS (asks it for its SyncInfo).
    pub async fn add_address(&self, host: &str, port: u16) -> anyhow::Result<String> {
        let mut info = self.inner.client.sync_info(host, port).await?;
        // CrossPaste only lists its addresses on the caller's subnet, so a
        // loopback query comes back empty: fall back to the address we used.
        if !info
            .endpoint_info
            .host_info_list
            .iter()
            .any(|h| h.host_address == host)
        {
            info.endpoint_info.host_info_list.insert(
                0,
                HostInfo {
                    network_prefix_length: 24,
                    host_address: host.to_string(),
                },
            );
        }
        let id = info.app_info.app_instance_id.clone();
        on_discovered(&self.inner, Discovered::Resolved(info)).await;
        Ok(id)
    }

    /// Initiator side of v2 pairing, step 1: asks `peer_id` to show its code.
    /// Returns the code we computed so callers can show it for comparison.
    pub async fn pair_start(&self, peer_id: &str) -> anyhow::Result<(u32, i64)> {
        let (host, port) = self.nearby_address(peer_id)?;
        let generation = now_ms();
        let req = self.exchange_request(generation);
        let resp = self.inner.client.exchange(&host, port, &req).await?;
        anyhow::ensure!(
            crypto::verify(
                &resp.sign_public_key,
                &crypto::key_exchange_message(
                    &resp.sign_public_key,
                    &resp.crypt_public_key,
                    resp.timestamp
                ),
                &resp.signature
            ),
            "peer signature invalid"
        );
        let sas = crypto::compute_sas(&self.inner.keys.crypt_public_der(), &resp.crypt_public_key);
        Ok((sas, generation))
    }

    /// Initiator side of v2 pairing, step 2: `code` is what the peer displays.
    pub async fn pair_finish(&self, peer_id: &str, code: u32) -> anyhow::Result<()> {
        let (host, port) = self.nearby_address(peer_id)?;
        let req = self.exchange_request(now_ms());
        let resp = self.inner.client.exchange(&host, port, &req).await?;
        let msg = crypto::key_exchange_message(
            &resp.sign_public_key,
            &resp.crypt_public_key,
            resp.timestamp,
        );
        anyhow::ensure!(
            crypto::verify(&resp.sign_public_key, &msg, &resp.signature),
            "peer signature invalid"
        );
        let sas = crypto::compute_sas(&self.inner.keys.crypt_public_der(), &resp.crypt_public_key);
        if sas != code {
            let _ = self.inner.client.cancel(&host, port, req.timestamp).await;
            anyhow::bail!("code does not match (possible interception)");
        }
        let ts = now_ms();
        let confirm = TrustConfirm {
            timestamp: ts,
            signature: self.inner.keys.sign(&crypto::trust_confirm_message(ts)),
        };
        let own_host = self
            .inner
            .sync_info_for(&host)
            .endpoint_info
            .host_info_list
            .first()
            .map(|h| h.host_address.clone())
            .unwrap_or_default();
        let own_info = self.inner.sync_info_for(&host);
        // Trust before confirming: the peer heartbeats us as soon as it has
        // stored our key, which can be before its confirm response reaches us.
        // Answering that heartbeat with DECRYPT_FAIL makes it drop the pairing.
        self.inner.trust(peer_id, &resp.crypt_public_key, None);
        let confirmed = async {
            let back = self
                .inner
                .client
                .confirm(&host, port, &own_host, &own_info, &confirm)
                .await?;
            anyhow::ensure!(
                crypto::verify(
                    &resp.sign_public_key,
                    &crypto::trust_confirm_message(back.timestamp),
                    &back.signature
                ),
                "peer confirm signature invalid"
            );
            Ok(())
        }
        .await;
        if let Err(e) = confirmed {
            self.inner.untrust(peer_id);
            return Err(e);
        }
        self.inner.emit(Event::Paired {
            peer_id: peer_id.to_string(),
            device_name: self.inner.device_name(peer_id),
        });
        self.inner.resolve(peer_id.to_string()).await;
        Ok(())
    }

    /// Answer to [`Event::PairingRequested`]. Rejecting also revokes a
    /// pairing the peer already completed.
    pub fn pairing_decision(&self, peer_id: &str, accept: bool) {
        if accept {
            return;
        }
        if let Some(p) = self.inner.pending.lock().unwrap().get_mut(peer_id) {
            p.rejected = true;
        }
        self.inner.untrust(peer_id);
    }

    pub fn unpair(&self, peer_id: &str) -> bool {
        self.inner.untrust(peer_id)
    }

    fn exchange_request(&self, timestamp: i64) -> KeyExchange {
        let sign_pub = self.inner.keys.sign_public_der();
        let crypt_pub = self.inner.keys.crypt_public_der();
        let signature = self.inner.keys.sign(&crypto::key_exchange_message(
            &sign_pub, &crypt_pub, timestamp,
        ));
        KeyExchange {
            sign_public_key: sign_pub,
            crypt_public_key: crypt_pub,
            timestamp,
            signature,
        }
    }

    fn nearby_address(&self, peer_id: &str) -> anyhow::Result<(String, u16)> {
        let info = self
            .inner
            .nearby
            .lock()
            .unwrap()
            .get(peer_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("device {peer_id} not found nearby"))?;
        let locals = self.inner.local_hosts.lock().unwrap().clone();
        let host = info
            .endpoint_info
            .host_info_list
            .iter()
            .find(|h| {
                locals.iter().any(|l| {
                    discovery::same_subnet(
                        &l.host_address,
                        &h.host_address,
                        l.network_prefix_length,
                    )
                })
            })
            .or(info.endpoint_info.host_info_list.first())
            .map(|h| h.host_address.clone())
            .ok_or_else(|| anyhow::anyhow!("device has no address"))?;
        Ok((host, info.endpoint_info.port))
    }

    pub fn shutdown(&self) {
        self.shutdown.notify_waiters();
        if let Some(d) = self.discovery.lock().unwrap().take() {
            d.shutdown();
        }
    }
}

async fn on_discovered(inner: &Arc<Inner>, event: Discovered) {
    match event {
        Discovered::Resolved(info) => {
            let id = info.app_info.app_instance_id.clone();
            let changed = inner
                .nearby
                .lock()
                .unwrap()
                .insert(id.clone(), info.clone())
                .as_ref()
                != Some(&info);
            if inner.is_trusted(&id) {
                inner.update_peer_info(info);
            }
            if changed {
                inner.emit(Event::DevicesChanged);
            }
        }
        Discovered::Removed(id) => {
            if inner.nearby.lock().unwrap().remove(&id).is_some() {
                inner.emit(Event::DevicesChanged);
            }
        }
    }
}

fn refresh_local_hosts(inner: &Arc<Inner>, discovery: &Arc<Mutex<Option<Discovery>>>) {
    if inner.cfg.advertise_hosts.is_some() {
        return;
    }
    let now = discovery::local_hosts();
    let changed = *inner.local_hosts.lock().unwrap() != now;
    if !changed {
        return;
    }
    info!(
        "crosspaste: local addresses changed to {:?}",
        now.iter().map(|h| &h.host_address).collect::<Vec<_>>()
    );
    *inner.local_hosts.lock().unwrap() = now;
    for c in inner.conns.lock().unwrap().values_mut() {
        c.advertised = false;
    }
    if let Some(d) = discovery.lock().unwrap().as_mut() {
        if let Err(e) = d.advertise(&inner.sync_info()) {
            warn!("crosspaste: re-advertise failed: {e:#}");
        }
    }
}

fn expire_pending(inner: &Arc<Inner>) {
    let expired: Vec<String> = {
        let mut pending = inner.pending.lock().unwrap();
        let ids: Vec<String> = pending
            .iter()
            .filter(|(_, p)| p.at.elapsed() > PENDING_EXCHANGE_TTL)
            .map(|(k, _)| k.clone())
            .collect();
        for id in &ids {
            pending.remove(id);
        }
        ids
    };
    for peer_id in expired {
        inner.emit(Event::PairingCancelled { peer_id });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_path_like_file_names() {
        assert!(is_safe_file_name("a.png"));
        assert!(!is_safe_file_name("../a.png"));
        assert!(!is_safe_file_name("C:a.png"));
        assert!(!is_safe_file_name(""));
    }
}
