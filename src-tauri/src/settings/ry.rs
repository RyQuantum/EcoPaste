//! RyQuantum/EcoPaste fork 专属设置，集中在 `ry` 段，方便叠到上游新版上。
//!
//! 与 `model.rs` 一样每个结构体都 `#[serde(default)]`：旧配置文件没有 `ry` 段时取默认值。

use serde::{Deserialize, Serialize};

/// 图片总占用上限默认 2 GB。
const DEFAULT_IMAGE_STORAGE_LIMIT_MB: u32 = 2048;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct Ry {
    /// 剪贴板窗口内快捷键的修饰键（仅 Windows）。`Alt` 表示 Alt 与 Ctrl 等效。
    /// 别名兼容 1.1.1-ry.3 / ry.4 写下的旧字段名。
    #[serde(alias = "quickPasteModifier")]
    pub window_modifier: WindowModifier,
    /// 拼接复制：按住 Ctrl 连按两下 C，把新复制的文本接到上一条历史后面（仅 Windows）。
    pub merge_copy: MergeCopy,
    /// 历史图片原图总占用上限，单位 MB；超过时删除最旧的普通图片，`0` = 不限。
    pub image_storage_limit_mb: u32,
}

impl Default for Ry {
    fn default() -> Self {
        Self {
            window_modifier: WindowModifier::Primary,
            merge_copy: MergeCopy::default(),
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

/// `Primary` 是平台主修饰键（macOS ⌘ / Windows Ctrl），即上游原有行为；
/// `Alt` 表示按住 Alt 与按住 Ctrl 完全等效（Ctrl 照常可用）。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum WindowModifier {
    #[default]
    Primary,
    Alt,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct MergeCopy {
    pub enabled: bool,
    pub separator: MergeSeparator,
    /// 拼接成功时播放提示音。
    pub sound: bool,
}

impl Default for MergeCopy {
    fn default() -> Self {
        Self {
            enabled: true,
            separator: MergeSeparator::Space,
            sound: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum MergeSeparator {
    #[default]
    Space,
    Newline,
}

impl MergeSeparator {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Space => " ",
            Self::Newline => "\n",
        }
    }
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
        assert!(settings.ry.merge_copy.enabled);
    }

    #[test]
    fn window_modifier_reads_the_old_quick_paste_field_name() {
        let ry: Ry = serde_json::from_str(r#"{ "quickPasteModifier": "alt" }"#).unwrap();

        assert_eq!(ry.window_modifier, WindowModifier::Alt);
        assert!(serde_json::to_string(&ry)
            .unwrap()
            .contains(r#""windowModifier":"alt""#));
    }

    #[test]
    fn zero_image_limit_means_unlimited() {
        let ry: Ry = serde_json::from_str(r#"{ "imageStorageLimitMb": 0 }"#).unwrap();

        assert_eq!(ry.image_storage_limit_bytes(), None);
    }
}
