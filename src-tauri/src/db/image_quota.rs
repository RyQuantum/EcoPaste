//! 按图片总占用清理历史（fork 补丁）。
//!
//! 与 [`crate::db::items::cleanup_history`] 的按时间、按条数规则并列：几种规则都生效，哪个先到按哪个删。
//! 总占用按库里图片记录的 `size`（原图 PNG 字节数）累加，置顶 / 收藏的图片也计入总量，但永不删除；
//! 超出上限时按 `created_at` 从旧到新删普通图片，删到总量不超过上限为止。
//! 缩略图（最长边 300px）不计入：它们懒生成、体积相对原图可以忽略。

use anyhow::Context;
use sqlx::SqlitePool;

use crate::core::Result;
use crate::db::items::CleanupOutcome;

/// 删除最旧的普通图片，直到图片总占用不超过 `limit_bytes`。返回删除行数与被删图片的文件名。
///
/// 窗口函数算出「删到这一行之前已经腾出的字节数」，只要还没腾够超出量，这一行就要删。
/// 总量恰好等于上限时不删。
pub async fn cleanup_images_over_quota(
    pool: &SqlitePool,
    limit_bytes: u64,
) -> Result<CleanupOutcome> {
    let limit = i64::try_from(limit_bytes).unwrap_or(i64::MAX);

    let rows = sqlx::query_as::<_, (String,)>(
        "DELETE FROM clipboard_items WHERE id IN ( \
             SELECT id FROM ( \
                 SELECT id, \
                        SUM(COALESCE(size, 0)) OVER ( \
                            ORDER BY created_at ASC, id ASC \
                            ROWS UNBOUNDED PRECEDING \
                        ) - COALESCE(size, 0) AS freed_before \
                 FROM clipboard_items \
                 WHERE kind = 'image' AND is_pinned = 0 AND is_favorite = 0 \
             ) \
             WHERE freed_before < ( \
                 SELECT COALESCE(SUM(size), 0) FROM clipboard_items WHERE kind = 'image' \
             ) - ? \
         ) \
         RETURNING content",
    )
    .bind(limit)
    .fetch_all(pool)
    .await
    .context("failed to cleanup clipboard images over storage quota")?;

    Ok(CleanupOutcome {
        removed: rows.len() as u64,
        image_files: rows.into_iter().map(|(content,)| content).collect(),
    })
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;

    use super::*;
    use crate::db::items::{content_hash, insert_item, query_items};
    use crate::db::models::{ClipboardItem, ClipboardItemQuery, ClipboardKind, Platform};
    use crate::db::test_support::memory_pool;

    const MB: i64 = 1024 * 1024;

    fn item(id: &str, kind: ClipboardKind, size: i64, created: i64) -> ClipboardItem {
        let ts = DateTime::from_timestamp(created, 0).unwrap();
        let content = match kind {
            ClipboardKind::Image => format!("{id}.png"),
            _ => format!("content-{id}"),
        };
        ClipboardItem {
            id: id.to_owned(),
            kind,
            sub_kind: None,
            group_id: None,
            source_app_id: None,
            content_hash: content_hash(kind, &content),
            content,
            search_text: None,
            summary: None,
            file_types: None,
            size: Some(size),
            width: None,
            height: None,
            use_count: 1,
            is_favorite: false,
            is_pinned: false,
            is_sensitive: false,
            platform: Platform::Windows,
            note: None,
            created_at: ts,
            updated_at: ts,
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

    fn image(id: &str, size_mb: i64, created: i64) -> ClipboardItem {
        item(id, ClipboardKind::Image, size_mb * MB, created)
    }

    async fn remaining_ids(pool: &SqlitePool) -> Vec<String> {
        let mut ids: Vec<String> = query_items(pool, &ClipboardItemQuery::default())
            .await
            .unwrap()
            .into_iter()
            .map(|item| item.id)
            .collect();
        ids.sort();
        ids
    }

    #[tokio::test]
    async fn deletes_oldest_images_until_total_fits() {
        let pool = memory_pool().await;
        // 四张 3 MB 图片共 12 MB，上限 7 MB：要腾出 5 MB，删最旧的两张（6 MB）后剩 6 MB。
        for (id, created) in [("a", 1), ("b", 2), ("c", 3), ("d", 4)] {
            insert_item(&pool, &image(id, 3, created)).await.unwrap();
        }

        let outcome = cleanup_images_over_quota(&pool, 7 * MB as u64)
            .await
            .unwrap();

        assert_eq!(outcome.removed, 2);
        assert_eq!(outcome.image_files, ["a.png", "b.png"]);
        assert_eq!(remaining_ids(&pool).await, ["c", "d"]);
    }

    #[tokio::test]
    async fn keeps_everything_when_total_is_at_or_under_limit() {
        let pool = memory_pool().await;
        insert_item(&pool, &image("a", 3, 1)).await.unwrap();
        insert_item(&pool, &image("b", 3, 2)).await.unwrap();

        let exact = cleanup_images_over_quota(&pool, 6 * MB as u64)
            .await
            .unwrap();
        let under = cleanup_images_over_quota(&pool, 100 * MB as u64)
            .await
            .unwrap();

        assert_eq!(exact.removed, 0);
        assert_eq!(under.removed, 0);
        assert_eq!(remaining_ids(&pool).await, ["a", "b"]);
    }

    #[tokio::test]
    async fn pinned_and_favorite_images_count_but_are_never_deleted() {
        let pool = memory_pool().await;
        let mut pinned = image("pinned", 4, 1);
        pinned.is_pinned = true;
        let mut favorite = image("favorite", 4, 2);
        favorite.is_favorite = true;
        insert_item(&pool, &pinned).await.unwrap();
        insert_item(&pool, &favorite).await.unwrap();
        insert_item(&pool, &image("old", 1, 3)).await.unwrap();
        insert_item(&pool, &image("new", 1, 4)).await.unwrap();

        // 总量 10 MB，上限 9 MB：受保护的 8 MB 不能动，只删最旧的普通图片 old。
        let outcome = cleanup_images_over_quota(&pool, 9 * MB as u64)
            .await
            .unwrap();
        assert_eq!(outcome.image_files, ["old.png"]);

        // 上限低于受保护图片总量：普通图片全删，受保护的仍保留。
        let outcome = cleanup_images_over_quota(&pool, MB as u64).await.unwrap();
        assert_eq!(outcome.image_files, ["new.png"]);
        assert_eq!(remaining_ids(&pool).await, ["favorite", "pinned"]);
    }

    #[tokio::test]
    async fn text_and_files_are_not_counted_or_deleted() {
        let pool = memory_pool().await;
        insert_item(&pool, &item("text", ClipboardKind::Text, 50 * MB, 1))
            .await
            .unwrap();
        insert_item(&pool, &item("files", ClipboardKind::Files, 50 * MB, 2))
            .await
            .unwrap();
        insert_item(&pool, &image("img", 2, 3)).await.unwrap();

        let outcome = cleanup_images_over_quota(&pool, 2 * MB as u64)
            .await
            .unwrap();

        assert_eq!(outcome.removed, 0);
        assert_eq!(remaining_ids(&pool).await, ["files", "img", "text"]);
    }
}
