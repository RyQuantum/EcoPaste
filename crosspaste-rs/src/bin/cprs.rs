//! `cprs`: a headless crosspaste-rs peer for interop tests.
//!
//! Usage: `cprs <data-dir> [port] [device-name]`
//! Events go to stdout as one JSON object per line; commands are read from
//! stdin, one per line:
//!   devices | add <host> <port> | text <s> | image <path> | pair <id> | pair-finish <id> <code>
//!   reject <id> | unpair <id> | quit

use std::io::BufRead;
use std::path::PathBuf;
use std::sync::Arc;

use crosspaste_rs::{Config, Event, Node};
use serde_json::json;

struct StderrLogger;

impl log::Log for StderrLogger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.target().starts_with("crosspaste")
            || m.level() <= log::Level::Warn
            || (m.target().starts_with("mdns_sd")
                && std::env::var("CPRS_DEBUG").as_deref() == Ok("mdns"))
    }
    fn log(&self, r: &log::Record) {
        if self.enabled(r.metadata()) {
            eprintln!("[{}] {}", r.level(), r.args());
        }
    }
    fn flush(&self) {}
}

fn emit(v: serde_json::Value) {
    println!("{v}");
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    log::set_logger(&StderrLogger).ok();
    let level = if std::env::var("CPRS_DEBUG").is_ok() {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    };
    log::set_max_level(level);

    let mut args = std::env::args().skip(1);
    let data_dir = PathBuf::from(args.next().unwrap_or_else(|| "cprs-data".into()));
    let port: u16 = args.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let name = args.next().unwrap_or_else(|| "cprs".into());

    let cfg = Config {
        data_dir,
        device_name: name,
        user_name: std::env::var("USERNAME")
            .or_else(|_| std::env::var("USER"))
            .unwrap_or_else(|_| "user".into()),
        app_version: "2.2.1".into(),
        app_revision: "crosspaste-rs".into(),
        port,
        platform: Config::platform_current(),
        bind_ip: None,
        advertise_hosts: None,
        mdns: true,
    };
    // Loopback-only mode: no mDNS, listen and advertise on 127.0.0.1 (no firewall prompt).
    let cfg = if std::env::var("CPRS_LOOPBACK").is_ok() {
        Config {
            bind_ip: Some([127, 0, 0, 1].into()),
            advertise_hosts: Some(vec![crosspaste_rs::model::HostInfo {
                network_prefix_length: 8,
                host_address: "127.0.0.1".into(),
            }]),
            mdns: false,
            ..cfg
        }
    } else {
        cfg
    };
    let sink = Arc::new(|e: Event| match e {
        Event::DevicesChanged => emit(json!({"event": "devices"})),
        Event::PairingRequested {
            peer_id,
            device_name,
            code,
        } => emit(
            json!({"event": "pairing", "peer": peer_id, "name": device_name, "code": format!("{code:06}")}),
        ),
        Event::Paired {
            peer_id,
            device_name,
        } => emit(json!({"event": "paired", "peer": peer_id, "name": device_name})),
        Event::PairingCancelled { peer_id } => {
            emit(json!({"event": "pairing-cancelled", "peer": peer_id}))
        }
        Event::Unpaired { peer_id } => emit(json!({"event": "unpaired", "peer": peer_id})),
        Event::RemoteText { peer_id, text } => {
            emit(json!({"event": "text", "peer": peer_id, "text": text}))
        }
        Event::RemoteImages { peer_id, paths } => {
            emit(json!({"event": "images", "peer": peer_id, "paths": paths}))
        }
    });
    let node = Node::start(cfg, sink).await?;
    emit(json!({"event": "started", "id": node.app_instance_id(), "port": node.port()}));

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    while let Some(line) = rx.recv().await {
        let line = line.trim().to_string();
        let (cmd, rest) = line.split_once(' ').unwrap_or((line.as_str(), ""));
        let result: anyhow::Result<()> = async {
            match cmd {
                "" => {}
                "devices" => {
                    for d in node.devices() {
                        emit(json!({
                            "device": d.app_instance_id, "name": d.device_name, "platform": d.platform,
                            "version": d.app_version, "paired": d.paired, "state": format!("{:?}", d.state), "host": d.host,
                        }));
                    }
                }
                "add" => {
                    let (host, port) = rest.split_once(' ').ok_or_else(|| anyhow::anyhow!("add <host> <port>"))?;
                    let id = node.add_address(host, port.trim().parse()?).await?;
                    emit(json!({"event": "added", "peer": id}));
                }
                "text" => {
                    node.send_text(rest).await;
                    emit(json!({"event": "sent"}));
                }
                "image" => {
                    let bytes = std::fs::read(rest)?;
                    let ext = if rest.to_ascii_lowercase().ends_with(".png") { "png" } else { "jpg" };
                    node.send_image(bytes, ext).await?;
                    emit(json!({"event": "sent"}));
                }
                "pair" => {
                    let (code, _) = node.pair_start(rest).await?;
                    emit(json!({"event": "pair-started", "code": format!("{code:06}")}));
                }
                "pair-finish" => {
                    let (id, code) = rest.split_once(' ').ok_or_else(|| anyhow::anyhow!("pair-finish <id> <code>"))?;
                    node.pair_finish(id, code.trim().parse()?).await?;
                    emit(json!({"event": "pair-finished", "peer": id}));
                }
                "reject" => node.pairing_decision(rest, false),
                "unpair" => {
                    node.unpair(rest);
                }
                "quit" => {
                    node.shutdown();
                    std::process::exit(0);
                }
                other => anyhow::bail!("unknown command {other}"),
            }
            Ok(())
        }
        .await;
        if let Err(e) = result {
            emit(json!({"event": "error", "message": format!("{e:#}")}));
        }
    }
    node.shutdown();
    Ok(())
}
