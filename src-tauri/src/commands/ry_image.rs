//! fork 补丁：「打开」条目。图片（右键菜单 / Ctrl+O / 中键）用截图工具，
//! 文字（中键）写临时文件后用设置里的编辑器，文件（中键）用默认程序打开第一个。

use tauri::{AppHandle, Manager, State};
use tauri_plugin_opener::OpenerExt;

use crate::clipboard::{open_image_file, open_text, ImageStore};
use crate::core::{AppError, Result};
use crate::db::items::find_item_by_id;
use crate::db::models::{ClipboardKind, ClipboardSubKind};
use crate::db::DatabaseState;
use crate::settings::SettingsStore;

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

/// 中键「打开」：按条目类型分派。
#[tauri::command]
pub async fn open_clipboard_item(
    app: AppHandle,
    db: State<'_, DatabaseState>,
    store: State<'_, ImageStore>,
    id: String,
) -> Result<()> {
    let pool = db.pool().await;
    let item = find_item_by_id(&pool, &id)
        .await?
        .ok_or_else(|| AppError::Clipboard(format!("clipboard item not found: {id}")))?;
    match item.kind {
        ClipboardKind::Image => open_clipboard_image(app, db, store, id).await,
        ClipboardKind::Text => {
            let text = match item.sub_kind {
                Some(ClipboardSubKind::Html | ClipboardSubKind::Rtf) => {
                    item.search_text.clone().unwrap_or(item.content)
                }
                _ => item.content,
            };
            let editor = app.state::<SettingsStore>().snapshot().ry.open_text_with;
            let opener_app = app.clone();
            let path = tauri::async_runtime::spawn_blocking(move || {
                open_text(&text, editor, |p| {
                    opener_app
                        .opener()
                        .open_path(p.to_string_lossy(), None::<&str>)
                        .map_err(|e| e.to_string())
                })
            })
            .await
            .map_err(|err| AppError::Clipboard(format!("open text task failed: {err}")))?
            .map_err(AppError::Clipboard)?;
            log::info!("opened clipboard text {id} with {editor:?} at {path:?}");
            Ok(())
        }
        ClipboardKind::Files => {
            let Some(first) = item.content.split('\n').find(|s| !s.is_empty()) else {
                return Ok(());
            };
            app.opener()
                .open_path(first, None::<&str>)
                .map_err(|e| AppError::Clipboard(e.to_string()))
        }
    }
}
