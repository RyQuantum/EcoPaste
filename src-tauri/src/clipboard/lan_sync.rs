//! 局域网同步（fork 补丁）：用 `crosspaste-rs` 和其他机器上的 CrossPaste 互传文字和图片。
//!
//! - 发出：本地新条目入库后（`merge.rs` 的两处入库点）交给 [`on_local_item`]，后台线程里
//!   读图、必要时转 JPEG，再发给所有已连接的设备。复制那一刻不做任何重活。
//! - 收到：自己入库，再经 [`write_to_clipboard`] 写进系统剪贴板；写回时登记的
//!   [`WritebackGuard`] 指纹让 watcher 跳过这次变化，所以收到的内容不会被当成新复制再发回去。
//! - 配对：对方设备（Mac / Deck 上的 CrossPaste）发起，这边弹窗显示 6 位数，在对方输入。
//!   弹窗点「拒绝」会撤销这次配对。
//!
//! 身份密钥放在固定启动目录 `<app_local_data>/<env>/crosspaste/`（DPAPI 加密），
//! 不跟随自定义数据目录搬走：它绑定这台机器的 Windows 账户。

use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;

use crosspaste_rs::{Config, Event, Node};
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

use super::guard::WritebackGuard;
use super::ingest::build_item_with_settings;
use super::payload::{ClipboardPayload, ImagePayload, TextPayload};
use super::storage::ImageStore;
use super::watcher::CLIPBOARD_UPDATED_EVENT;
use super::write::write_to_clipboard;
use crate::db::items::upsert_item;
use crate::db::models::{ClipboardItem, ClipboardKind, ClipboardSubKind};
use crate::settings::SettingsStore;

/// 首选端口；被占用时退回随机端口（对方经 mDNS 和心跳得知新端口）。
const PREFERRED_PORT: u16 = 13130;
/// 发出的图片超过这个大小、且没有透明像素时转成 JPEG。
const JPEG_THRESHOLD_BYTES: usize = 500 * 1024;
const JPEG_QUALITY: u8 = 90;

/// 运行中的同步节点，注册在 Tauri `State` 里。
pub struct LanSync {
    node: Node,
}

/// 设置里开着同步时启动节点。失败只记日志，不影响剪贴板本身。
pub fn init(app: &AppHandle) {
    let settings = app
        .try_state::<SettingsStore>()
        .map(|s| s.snapshot())
        .unwrap_or_default();
    if !settings.ry.lan_sync.enabled {
        log::info!("lan sync disabled in settings");
        return;
    }
    let data_dir = match crate::core::paths::bootstrap_dir(app) {
        Ok(dir) => dir.join("crosspaste"),
        Err(err) => {
            log::error!("lan sync: resolve data dir failed: {err}");
            return;
        }
    };
    let cfg = Config {
        data_dir,
        device_name: format!("{} (EcoPaste)", computer_name()),
        user_name: std::env::var("USERNAME")
            .or_else(|_| std::env::var("USER"))
            .unwrap_or_else(|_| "user".into()),
        app_version: app.package_info().version.to_string(),
        app_revision: "EcoPaste".into(),
        port: PREFERRED_PORT,
        platform: Config::platform_current(),
        bind_ip: None,
        advertise_hosts: None,
        mdns: true,
    };

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let sink_app = app.clone();
        let sink = Arc::new(move |event: Event| on_event(&sink_app, event));
        match Node::start(cfg, sink).await {
            Ok(node) => {
                log::info!(
                    "lan sync started as {} on port {}",
                    node.app_instance_id(),
                    node.port()
                );
                app.manage(LanSync { node });
            }
            Err(err) => log::error!("lan sync: start failed: {err:#}"),
        }
    });
}

/// 本地新条目入库后调用：文字和图片发给已连接的设备。文件不同步；
/// 命中敏感内容规则的条目（密钥、密码之类）也不发出去。
pub fn on_local_item(app: &AppHandle, item: &ClipboardItem) {
    if item.is_sensitive {
        return;
    }
    let Some(state) = app.try_state::<LanSync>() else {
        return;
    };
    let node = state.node.clone();
    match item.kind {
        ClipboardKind::Text => {
            let text = match item.sub_kind {
                Some(ClipboardSubKind::Html | ClipboardSubKind::Rtf) => item
                    .search_text
                    .clone()
                    .unwrap_or_else(|| item.content.clone()),
                _ => item.content.clone(),
            };
            tauri::async_runtime::spawn(async move { node.send_text(&text).await });
        }
        ClipboardKind::Image => {
            if !node.has_connected_peers() {
                return;
            }
            let Some(store) = app.try_state::<ImageStore>() else {
                return;
            };
            let path = store.origin_path(&item.content);
            tauri::async_runtime::spawn(async move {
                let prepared =
                    tauri::async_runtime::spawn_blocking(move || prepare_image(&path)).await;
                match prepared {
                    Ok(Ok((bytes, ext))) => {
                        if let Err(err) = node.send_image(bytes, ext).await {
                            log::warn!("lan sync: send image failed: {err:#}");
                        }
                    }
                    Ok(Err(err)) => log::warn!("lan sync: prepare image failed: {err}"),
                    Err(err) => log::warn!("lan sync: prepare image task failed: {err}"),
                }
            });
        }
        ClipboardKind::Files => {}
    }
}

/// 读出历史里存的 PNG 原图；超过阈值且不透明时转成高质量 JPEG。
/// 对方的 Java 图片库读得了 JPEG、读不了 WebP。
fn prepare_image(path: &PathBuf) -> Result<(Vec<u8>, &'static str), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{path:?}: {e}"))?;
    if bytes.len() <= JPEG_THRESHOLD_BYTES {
        return Ok((bytes, "png"));
    }
    let image = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    if has_transparency(&image) {
        return Ok((bytes, "png"));
    }
    let mut out = Cursor::new(Vec::new());
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY);
    image::DynamicImage::ImageRgb8(image.to_rgb8())
        .write_with_encoder(encoder)
        .map_err(|e| e.to_string())?;
    let jpeg = out.into_inner();
    if jpeg.len() >= bytes.len() {
        return Ok((bytes, "png"));
    }
    log::info!(
        "lan sync: image {} KB -> JPEG {} KB",
        bytes.len() / 1024,
        jpeg.len() / 1024
    );
    Ok((jpeg, "jpg"))
}

fn has_transparency(image: &image::DynamicImage) -> bool {
    image.color().has_alpha() && image.to_rgba8().pixels().any(|p| p.0[3] != 255)
}

fn on_event(app: &AppHandle, event: Event) {
    match event {
        Event::PairingRequested {
            peer_id,
            device_name,
            code,
        } => show_pairing_dialog(app, peer_id, device_name, code),
        Event::Paired {
            device_name,
            peer_id,
        } => log::info!("lan sync: paired with {device_name} ({peer_id})"),
        Event::Unpaired { peer_id } => log::info!("lan sync: unpaired {peer_id}"),
        Event::RemoteText { text, .. } => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move { receive_text(&app, text).await });
        }
        Event::RemoteImages { paths, .. } => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                for path in paths {
                    receive_image(&app, path).await;
                }
            });
        }
        Event::DevicesChanged | Event::PairingCancelled { .. } => {}
    }
}

fn show_pairing_dialog(app: &AppHandle, peer_id: String, device_name: String, code: u32) {
    let digits = format!("{code:06}");
    let message = format!(
        "「{device_name}」请求和 EcoPaste 配对。\n\n请在对方设备上输入：{} {}\n\n\
         对方显示配对成功后点「完成」。不是你发起的就点「拒绝」，会同时撤销这次配对。",
        &digits[..3],
        &digits[3..]
    );
    let app_for_answer = app.clone();
    app.dialog()
        .message(message)
        .title("局域网同步：配对")
        .kind(MessageDialogKind::Info)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "完成".into(),
            "拒绝".into(),
        ))
        .show(move |accepted| {
            if let Some(state) = app_for_answer.try_state::<LanSync>() {
                state.node.pairing_decision(&peer_id, accepted);
            }
        });
}

fn settings_snapshot(app: &AppHandle) -> crate::settings::Settings {
    app.try_state::<SettingsStore>()
        .map(|s| s.snapshot())
        .unwrap_or_default()
}

async fn receive_text(app: &AppHandle, text: String) {
    let payload = ClipboardPayload::Text(TextPayload {
        text,
        html: None,
        rtf: None,
    });
    persist_and_write(app, payload).await;
}

async fn receive_image(app: &AppHandle, path: PathBuf) {
    let decoded = tauri::async_runtime::spawn_blocking(move || -> Result<ImagePayload, String> {
        let bytes = std::fs::read(&path).map_err(|e| format!("{path:?}: {e}"))?;
        let format = image::guess_format(&bytes).map_err(|e| e.to_string())?;
        if format == image::ImageFormat::Png {
            let (width, height) = image::ImageReader::with_format(Cursor::new(&bytes), format)
                .into_dimensions()
                .map_err(|e| e.to_string())?;
            return Ok(ImagePayload {
                bytes,
                width,
                height,
            });
        }
        // 历史库只存 PNG：JPEG 等先转码。
        let image = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
        let mut png = Cursor::new(Vec::new());
        image
            .write_to(&mut png, image::ImageFormat::Png)
            .map_err(|e| e.to_string())?;
        Ok(ImagePayload {
            bytes: png.into_inner(),
            width: image.width(),
            height: image.height(),
        })
    })
    .await;
    match decoded {
        Ok(Ok(payload)) => {
            persist_and_write(app, ClipboardPayload::Image(payload)).await;
            super::image_quota::enforce(app).await;
        }
        Ok(Err(err)) => log::warn!("lan sync: received image unusable: {err}"),
        Err(err) => log::warn!("lan sync: decode task failed: {err}"),
    }
}

/// 入库、写进系统剪贴板、提示音。
async fn persist_and_write(app: &AppHandle, payload: ClipboardPayload) {
    let settings = settings_snapshot(app);
    let Some(store) = app.try_state::<ImageStore>().map(|s| s.inner().clone()) else {
        return;
    };
    let item = match build_item_with_settings(
        &store,
        &payload,
        &settings.clipboard.capture,
        &settings.clipboard.sensitive,
        true,
    ) {
        Ok(Some(item)) => item,
        Ok(None) => {
            log::info!("lan sync: received content filtered by capture settings");
            return;
        }
        Err(err) => {
            log::warn!("lan sync: build received item failed: {err}");
            return;
        }
    };

    let pool = app.state::<crate::db::DatabaseState>().pool().await;
    match upsert_item(&pool, &item).await {
        Ok(result) => {
            if let Err(err) = app.emit(
                CLIPBOARD_UPDATED_EVENT,
                json!({ "id": result.id, "kind": item.kind, "deduplicated": result.deduplicated }),
            ) {
                log::warn!("emit {CLIPBOARD_UPDATED_EVENT} failed: {err}");
            }
        }
        Err(err) => log::warn!("lan sync: persist received item failed: {err}"),
    }

    let guard = app
        .try_state::<Arc<WritebackGuard>>()
        .map(|g| g.inner().clone());
    if let Some(guard) = guard {
        let written = tauri::async_runtime::spawn_blocking(move || {
            write_to_clipboard(&store, &guard, &item, false)
        })
        .await;
        match written {
            Ok(Ok(())) => {}
            Ok(Err(err)) => log::warn!("lan sync: write received item to clipboard failed: {err}"),
            Err(err) => log::warn!("lan sync: clipboard write task failed: {err}"),
        }
    }

    if settings.ry.lan_sync.sound {
        super::merge_sound::play_receive(app);
    }
}

fn computer_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "EcoPaste".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_png(dir: &std::path::Path, name: &str, image: image::DynamicImage) -> PathBuf {
        let path = dir.join(name);
        image
            .save_with_format(&path, image::ImageFormat::Png)
            .unwrap();
        path
    }

    fn noise(width: u32, height: u32, alpha: Option<u8>) -> image::DynamicImage {
        let mut seed = 0x2545_f491_u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed & 0xff) as u8
        };
        match alpha {
            None => {
                image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(width, height, |_, _| {
                    image::Rgb([next(), next(), next()])
                }))
            }
            Some(a) => {
                image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(width, height, |_, _| {
                    image::Rgba([next(), next(), next(), a])
                }))
            }
        }
    }

    #[test]
    fn small_images_are_sent_as_png_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_png(dir.path(), "small.png", noise(64, 64, None));
        let (bytes, ext) = prepare_image(&path).unwrap();
        assert_eq!(ext, "png");
        assert_eq!(bytes, std::fs::read(&path).unwrap());
    }

    #[test]
    fn large_opaque_images_become_jpeg() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_png(dir.path(), "big.png", noise(600, 600, Some(255)));
        assert!(std::fs::metadata(&path).unwrap().len() as usize > JPEG_THRESHOLD_BYTES);
        let (bytes, ext) = prepare_image(&path).unwrap();
        assert_eq!(ext, "jpg");
        assert_eq!(
            image::guess_format(&bytes).unwrap(),
            image::ImageFormat::Jpeg
        );
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (600, 600));
    }

    #[test]
    fn large_transparent_images_stay_png() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_png(dir.path(), "alpha.png", noise(600, 600, Some(128)));
        assert!(std::fs::metadata(&path).unwrap().len() as usize > JPEG_THRESHOLD_BYTES);
        let (_, ext) = prepare_image(&path).unwrap();
        assert_eq!(ext, "png");
    }
}
