//! 取「最近一条历史」（fork 补丁，拼接复制用）。

use anyhow::Context;
use sqlx::SqlitePool;

use crate::core::Result;
use crate::db::items::find_item_by_id;
use crate::db::models::ClipboardItem;

/// 按最近使用时间取最新一条记录，不区分置顶：拼接复制要接在用户刚复制过的那条后面。
/// 同内容再次复制会刷新 `updated_at`（见 `upsert_item`），所以重复复制的旧条目也算「最近一条」。
pub async fn find_latest_item(pool: &SqlitePool) -> Result<Option<ClipboardItem>> {
    let id = sqlx::query_scalar::<_, String>(
        "SELECT id FROM clipboard_items ORDER BY updated_at DESC, created_at DESC LIMIT 1",
    )
    .fetch_optional(pool)
    .await
    .context("failed to find latest clipboard item")?;

    match id {
        Some(id) => find_item_by_id(pool, &id).await,
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;

    use super::*;
    use crate::db::items::{content_hash, insert_item};
    use crate::db::models::{ClipboardKind, Platform};
    use crate::db::test_support::memory_pool;

    fn item(id: &str, created: i64, updated: i64) -> ClipboardItem {
        let content = format!("content-{id}");
        ClipboardItem {
            id: id.to_owned(),
            kind: ClipboardKind::Text,
            sub_kind: None,
            group_id: None,
            source_app_id: None,
            content_hash: content_hash(ClipboardKind::Text, &content),
            content,
            search_text: None,
            summary: None,
            file_types: None,
            size: None,
            width: None,
            height: None,
            use_count: 1,
            is_favorite: false,
            is_pinned: false,
            is_sensitive: false,
            platform: Platform::Windows,
            note: None,
            created_at: DateTime::from_timestamp(created, 0).unwrap(),
            updated_at: DateTime::from_timestamp(updated, 0).unwrap(),
            source_app_name: None,
            source_app_icon_file: None,
            source_app_icon_path: None,
            image_thumbnail_path: None,
            file_entries: None,
            files_preview_kind: None,
            available_actions: Vec::new(),
            color_preview: None,
            display_created_at: String::new(),
        }
    }

    #[tokio::test]
    async fn returns_none_on_empty_history() {
        let pool = memory_pool().await;

        assert!(find_latest_item(&pool).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn picks_most_recently_used_item_even_if_pinned_or_older() {
        let pool = memory_pool().await;
        insert_item(&pool, &item("newest-created", 300, 300))
            .await
            .unwrap();
        // 创建得早，但刚被重新复制过（updated_at 最新），应当是「最近一条」。
        insert_item(&pool, &item("recopied", 100, 400))
            .await
            .unwrap();
        let mut pinned = item("pinned", 200, 200);
        pinned.is_pinned = true;
        insert_item(&pool, &pinned).await.unwrap();

        let latest = find_latest_item(&pool).await.unwrap().unwrap();

        assert_eq!(latest.id, "recopied");
    }
}
