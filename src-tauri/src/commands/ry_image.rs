//! fork 补丁：图片条目的「打开」（右键菜单 / Ctrl+O）。

use tauri::{AppHandle, State};
use tauri_plugin_opener::OpenerExt;

use crate::clipboard::{open_image_file, ImageStore};
use crate::core::{AppError, Result};
use crate::db::items::find_item_by_id;
use crate::db::models::ClipboardKind;
use crate::db::DatabaseState;

/// 用截图工具（Windows）或默认看图程序打开图片条目的原图。
#[tauri::command]
pub async fn open_clipboard_image(
    app: AppHandle,
    db: State<'_, DatabaseState>,
    store: State<'_, ImageStore>,
    id: String,
) -> Result<()> {
    let pool = db.pool().await;
    let item = find_item_by_id(&pool, &id)
        .await?
        .ok_or_else(|| AppError::Clipboard(format!("clipboard item not found: {id}")))?;
    if item.kind != ClipboardKind::Image || item.content.contains(['/', '\\']) {
        return Err(AppError::Clipboard(
            "selected item is not an image".to_owned(),
        ));
    }
    let path = store.origin_path(&item.content);
    if !path.is_file() {
        return Err(AppError::Clipboard("image file does not exist".to_owned()));
    }

    let how = tauri::async_runtime::spawn_blocking(move || {
        open_image_file(&path, |p| {
            app.opener()
                .open_path(p.to_string_lossy(), None::<&str>)
                .map_err(|e| e.to_string())
        })
    })
    .await
    .map_err(|err| AppError::Clipboard(format!("open image task failed: {err}")))?
    .map_err(AppError::Clipboard)?;
    log::info!("opened clipboard image {id} with {how}");
    Ok(())
}
