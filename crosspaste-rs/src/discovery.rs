//! mDNS advertisement and browsing of `_crosspasteService._tcp`.
//!
//! The TXT record carries our `SyncInfo` as base64(JSON), split into
//! 128-char chunks under keys "0", "1", ... (`TxtRecordUtils`).

use std::collections::HashMap;
use std::net::Ipv4Addr;

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};

use crate::model::{HostInfo, SyncInfo};

pub const SERVICE_TYPE: &str = "_crosspasteService._tcp.local.";

pub fn encode_txt(sync_info: &SyncInfo) -> HashMap<String, String> {
    let encoded = B64.encode(serde_json::to_vec(sync_info).expect("serialize SyncInfo"));
    encoded
        .as_bytes()
        .chunks(128)
        .enumerate()
        .map(|(i, c)| (i.to_string(), String::from_utf8_lossy(c).into_owned()))
        .collect()
}

pub fn decode_txt(props: &[(String, String)]) -> Option<SyncInfo> {
    let mut parts: Vec<(usize, &str)> = props
        .iter()
        .filter_map(|(k, v)| k.parse::<usize>().ok().map(|i| (i, v.as_str())))
        .collect();
    if parts.is_empty() {
        return None;
    }
    parts.sort_by_key(|(i, _)| *i);
    let joined: String = parts.into_iter().map(|(_, v)| v).collect();
    let json = B64.decode(joined.as_bytes()).ok()?;
    serde_json::from_slice(&json).ok()
}

/// `crosspaste@<appInstanceId>@<ip with _>` -> appInstanceId.
pub fn app_instance_id_from_name(name: &str) -> Option<String> {
    let instance = name
        .strip_suffix(SERVICE_TYPE)
        .unwrap_or(name)
        .trim_end_matches('.');
    let parts: Vec<&str> = instance.split('@').collect();
    (parts.len() == 3 && parts[0] == "crosspaste" && !parts[1].is_empty() && !parts[2].is_empty())
        .then(|| parts[1].to_string())
}

const VIRTUAL_ADAPTER_HINTS: [&str; 7] = [
    "vethernet",
    "hyper-v",
    "wsl",
    "virtualbox",
    "vmware",
    "docker",
    "loopback",
];

/// Local IPv4 addresses worth advertising: up, not loopback, not link-local,
/// not a virtual switch (Hyper-V/WSL and friends).
pub fn local_hosts() -> Vec<HostInfo> {
    let mut out = Vec::new();
    for iface in if_addrs::get_if_addrs().unwrap_or_default() {
        let name = iface.name.to_ascii_lowercase();
        if iface.is_loopback() || VIRTUAL_ADAPTER_HINTS.iter().any(|h| name.contains(h)) {
            continue;
        }
        if let if_addrs::IfAddr::V4(v4) = &iface.addr {
            if v4.ip.is_link_local() {
                continue;
            }
            let prefix = u32::from(v4.netmask).count_ones() as i16;
            out.push(HostInfo {
                network_prefix_length: prefix,
                host_address: v4.ip.to_string(),
            });
        }
    }
    out.sort_by(|a, b| a.host_address.cmp(&b.host_address));
    out.dedup();
    out
}

/// Whether `a` and `b` are in the same IPv4 subnet of `prefix` bits.
pub fn same_subnet(a: &str, b: &str, prefix: i16) -> bool {
    let (Ok(a), Ok(b)) = (a.parse::<Ipv4Addr>(), b.parse::<Ipv4Addr>()) else {
        return false;
    };
    let prefix = prefix.clamp(0, 32) as u32;
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    (u32::from(a) & mask) == (u32::from(b) & mask)
}

#[allow(clippy::large_enum_variant)]
pub enum Discovered {
    Resolved(SyncInfo),
    Removed(String),
}

pub struct Discovery {
    daemon: ServiceDaemon,
    registered: Vec<String>,
}

impl Discovery {
    pub fn new() -> anyhow::Result<Self> {
        let daemon = ServiceDaemon::new()?;
        // "_crosspasteService" is 17 bytes; RFC 6763 (and mdns-sd by default)
        // caps service names at 15, and registration would silently fail.
        daemon.set_service_name_len_max(20)?;
        Ok(Self {
            daemon,
            registered: Vec::new(),
        })
    }

    /// (Re)registers one service per advertised address, like CrossPaste does.
    pub fn advertise(&mut self, sync_info: &SyncInfo) -> anyhow::Result<()> {
        for fullname in self.registered.drain(..) {
            let _ = self.daemon.unregister(&fullname);
        }
        let props = encode_txt(sync_info);
        let id = &sync_info.app_info.app_instance_id;
        for host in &sync_info.endpoint_info.host_info_list {
            let name = format!("crosspaste@{}@{}", id, host.host_address.replace('.', "_"));
            let host_name = format!("ecopaste-{}.local.", &id[..8.min(id.len())]);
            let info = ServiceInfo::new(
                SERVICE_TYPE,
                &name,
                &host_name,
                host.host_address.as_str(),
                sync_info.endpoint_info.port,
                props.clone(),
            )?;
            self.registered.push(info.get_fullname().to_string());
            self.daemon.register(info)?;
        }
        Ok(())
    }

    /// Browses for peers; `on_event` runs on a dedicated thread.
    pub fn browse(
        &self,
        own_id: String,
        on_event: impl Fn(Discovered) + Send + 'static,
    ) -> anyhow::Result<()> {
        let rx = self.daemon.browse(SERVICE_TYPE)?;
        std::thread::Builder::new()
            .name("crosspaste-mdns".into())
            .spawn(move || {
                while let Ok(event) = rx.recv() {
                    match event {
                        ServiceEvent::ServiceResolved(info) => {
                            let props: Vec<(String, String)> = info
                                .get_properties()
                                .iter()
                                .map(|p| (p.key().to_string(), p.val_str().to_string()))
                                .collect();
                            if let Some(sync_info) = decode_txt(&props) {
                                if sync_info.app_info.app_instance_id != own_id {
                                    on_event(Discovered::Resolved(sync_info));
                                }
                            }
                        }
                        ServiceEvent::ServiceRemoved(_, fullname) => {
                            if let Some(id) = app_instance_id_from_name(&fullname) {
                                if id != own_id {
                                    on_event(Discovered::Removed(id));
                                }
                            }
                        }
                        _ => {}
                    }
                }
            })?;
        Ok(())
    }

    pub fn shutdown(&self) {
        for fullname in &self.registered {
            let _ = self.daemon.unregister(fullname);
        }
        let _ = self.daemon.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AppInfo, EndpointInfo, Platform};

    fn sample() -> SyncInfo {
        SyncInfo {
            app_info: AppInfo {
                app_instance_id: "a".repeat(36),
                app_version: "2.2.1".into(),
                app_revision: "EcoPaste".into(),
                user_name: "Ryan".into(),
                pairing_version: Some(2),
            },
            endpoint_info: EndpointInfo {
                device_id: "d".into(),
                device_name: "RYAN-MINIPC".into(),
                platform: Platform {
                    name: "Windows".into(),
                    arch: "amd64".into(),
                    bit_mode: 64,
                    version: "11".into(),
                },
                host_info_list: vec![HostInfo {
                    network_prefix_length: 24,
                    host_address: "10.0.0.199".into(),
                }],
                port: 13130,
            },
        }
    }

    #[test]
    fn txt_round_trip_with_unordered_keys() {
        let txt = encode_txt(&sample());
        assert!(txt.len() > 1, "payload should need several chunks");
        assert!(txt.values().all(|v| v.len() <= 128));
        let mut props: Vec<(String, String)> = txt.into_iter().collect();
        props.reverse();
        assert_eq!(decode_txt(&props), Some(sample()));
    }

    #[test]
    fn decodes_a_real_crosspaste_record() {
        // The Mac's SyncInfo as JSON, as resolved by CrossPaste 2.2.1 on 2026-10-16.
        let json = r#"{"appInfo":{"appInstanceId":"B4082156-F6FF-5BDC-99F4-33366F9E3390","appVersion":"2.2.1","appRevision":"2706","userName":"ryan","pairingVersion":3},"endpointInfo":{"deviceId":"B4082156-F6FF-5BDC-99F4-33366F9E3390","deviceName":"Ryan’s MacBook Air","platform":{"name":"Macos","arch":"aarch64","bitMode":64,"version":"27.0.1"},"hostInfoList":[{"networkPrefixLength":24,"hostAddress":"10.0.0.243"}],"port":13129}}"#;
        let props = vec![("0".to_string(), B64.encode(json))];
        let info = decode_txt(&props).unwrap();
        assert_eq!(info.endpoint_info.port, 13129);
        assert_eq!(info.app_info.pairing_version, Some(3));
    }

    #[test]
    fn service_name_parsing() {
        assert_eq!(
            app_instance_id_from_name("crosspaste@abc@10_0_0_1._crosspasteService._tcp.local."),
            Some("abc".into())
        );
        assert_eq!(app_instance_id_from_name("other@abc"), None);
    }

    #[test]
    fn subnet_match() {
        assert!(same_subnet("10.0.0.199", "10.0.0.243", 24));
        assert!(!same_subnet("10.0.0.199", "10.0.1.243", 24));
        assert!(!same_subnet("10.0.0.199", "not-an-ip", 24));
    }
}
