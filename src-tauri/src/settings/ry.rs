//! RyQuantum/EcoPaste fork 专属设置，集中在 `ry` 段，方便叠到上游新版上。
//!
//! 与 `model.rs` 一样每个结构体都 `#[serde(default)]`：旧配置文件没有 `ry` 段时取默认值。

use serde::{Deserialize, Serialize};

/// 图片总占用上限默认 2 GB。
const DEFAULT_IMAGE_STORAGE_LIMIT_MB: u32 = 2048;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct Ry {
    /// 列表「修饰键 + 数字」快速粘贴使用的修饰键。
    pub quick_paste_modifier: QuickPasteModifier,
    /// 历史图片原图总占用上限，单位 MB；超过时删除最旧的普通图片，`0` = 不限。
    pub image_storage_limit_mb: u32,
}

impl Default for Ry {
    fn default() -> Self {
        Self {
            quick_paste_modifier: QuickPasteModifier::Primary,
            image_storage_limit_mb: DEFAULT_IMAGE_STORAGE_LIMIT_MB,
        }
    }
}

impl Ry {
    /// 返回图片总占用上限的字节数；`None` 表示不限。
    pub fn image_storage_limit_bytes(&self) -> Option<u64> {
        if self.image_storage_limit_mb == 0 {
            return None;
        }

        Some(u64::from(self.image_storage_limit_mb) * 1024 * 1024)
    }
}

/// `Primary` 是平台主修饰键（macOS ⌘ / Windows Ctrl），即上游原有行为。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum QuickPasteModifier {
    #[default]
    Primary,
    Alt,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_ry_section_falls_back_to_defaults() {
        let settings: crate::settings::Settings = serde_json::from_str("{}").unwrap();

        assert_eq!(settings.ry, Ry::default());
        assert_eq!(
            settings.ry.image_storage_limit_bytes(),
            Some(2048 * 1024 * 1024)
        );
    }

    #[test]
    fn zero_image_limit_means_unlimited() {
        let ry: Ry = serde_json::from_str(r#"{ "imageStorageLimitMb": 0 }"#).unwrap();

        assert_eq!(ry.image_storage_limit_bytes(), None);
    }
}
