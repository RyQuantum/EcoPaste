//! 图片总占用上限的执行入口（fork 补丁）：读设置 → 删超额的旧图片 → 删落盘文件 → 通知前端刷新。
//!
//! 启动清理、周期清理（见 `cleanup.rs`）之后各跑一次，每张新图片入库后也跑一次：
//! 上游默认不开周期清理，只靠启动时清理的话，上限要等下次重启才生效。

use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};

use super::cleanup::remove_images;
use super::watcher::CLIPBOARD_UPDATED_EVENT;
use crate::db::image_quota::cleanup_images_over_quota;
use crate::settings::SettingsStore;

/// 按当前设置执行一次图片总占用清理；上限为 0（不限）时直接返回。失败只记日志。
pub async fn enforce(app: &AppHandle) {
    let Some(limit) = app
        .try_state::<SettingsStore>()
        .and_then(|store| store.snapshot().ry.image_storage_limit_bytes())
    else {
        return;
    };

    let pool = app.state::<crate::db::DatabaseState>().pool().await;
    match cleanup_images_over_quota(&pool, limit).await {
        Ok(outcome) if outcome.removed == 0 => {}
        Ok(outcome) => {
            remove_images(app, &outcome.image_files);
            log::info!(
                "image quota cleanup removed {} image(s) to stay under {limit} bytes",
                outcome.removed
            );
            if let Err(err) = app.emit(
                CLIPBOARD_UPDATED_EVENT,
                json!({ "cleanup": outcome.removed }),
            ) {
                log::warn!("emit image quota cleanup event failed: {err}");
            }
        }
        Err(err) => log::warn!("image quota cleanup failed: {err}"),
    }
}
