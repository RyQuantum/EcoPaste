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
    /// 局域网同步：和其他机器上的 CrossPaste 互传文字和图片（`crosspaste-rs`）。
    pub lan_sync: LanSync,
    /// 中键「打开」文字时用的编辑器（图片固定用截图工具）。
    pub open_text_with: OpenTextApp,
}

impl Default for Ry {
    fn default() -> Self {
        Self {
            window_modifier: WindowModifier::Primary,
            merge_copy: MergeCopy::default(),
            image_storage_limit_mb: DEFAULT_IMAGE_STORAGE_LIMIT_MB,
            lan_sync: LanSync::default(),
            open_text_with: OpenTextApp::default(),
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct LanSync {
    /// 启动时开启同步服务；改动后重启 EcoPaste 生效。
    pub enabled: bool,
    /// 收到其他设备的内容时播放提示音。ry.9 ~ ry.11 存的是布尔值，读取时兼容。
    pub sound: ReceiveSound,
}

impl Default for LanSync {
    fn default() -> Self {
        Self {
            enabled: true,
            sound: ReceiveSound::Images,
        }
    }
}

/// 打开文字用的程序。`Default` 是系统给 .txt 关联的程序。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum OpenTextApp {
    #[default]
    Notepad,
    Vscode,
    Neovim,
    Default,
}

/// 收到内容时什么情况下播放提示音。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", from = "ReceiveSoundRepr")]
pub enum ReceiveSound {
    Off,
    /// 只在收到图片时响；文字静默。
    #[default]
    Images,
    All,
}

impl ReceiveSound {
    pub fn plays_for_image(self) -> bool {
        self != Self::Off
    }

    pub fn plays_for_text(self) -> bool {
        self == Self::All
    }
}

/// 旧版本写下的 `true` / `false` 和新版本的字符串都能读。
#[derive(Deserialize)]
#[serde(untagged)]
enum ReceiveSoundRepr {
    Legacy(bool),
    Named(ReceiveSoundName),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum ReceiveSoundName {
    Off,
    Images,
    All,
}

impl From<ReceiveSoundRepr> for ReceiveSound {
    fn from(repr: ReceiveSoundRepr) -> Self {
        match repr {
            ReceiveSoundRepr::Legacy(true) => Self::Images,
            ReceiveSoundRepr::Legacy(false) => Self::Off,
            ReceiveSoundRepr::Named(ReceiveSoundName::Off) => Self::Off,
            ReceiveSoundRepr::Named(ReceiveSoundName::Images) => Self::Images,
            ReceiveSoundRepr::Named(ReceiveSoundName::All) => Self::All,
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
        assert!(settings.ry.lan_sync.enabled);
        assert_eq!(settings.ry.lan_sync.sound, ReceiveSound::Images);
    }

    #[test]
    fn receive_sound_reads_the_old_boolean_and_writes_a_name() {
        let on: LanSync = serde_json::from_str(r#"{ "enabled": true, "sound": true }"#).unwrap();
        let off: LanSync = serde_json::from_str(r#"{ "sound": false }"#).unwrap();
        let all: LanSync = serde_json::from_str(r#"{ "sound": "all" }"#).unwrap();

        assert_eq!(on.sound, ReceiveSound::Images);
        assert_eq!(off.sound, ReceiveSound::Off);
        assert_eq!(all.sound, ReceiveSound::All);
        assert!(serde_json::to_string(&on)
            .unwrap()
            .contains(r#""sound":"images""#));
        assert!(!ReceiveSound::Images.plays_for_text());
        assert!(ReceiveSound::Images.plays_for_image());
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
    fn middle_click_open_from_ry14_reads_as_edit() {
        let action: crate::settings::MiddleClickAction =
            serde_json::from_str(r#""singleClickOpen""#).unwrap();

        assert_eq!(action, crate::settings::MiddleClickAction::SingleClickEdit);
        assert_eq!(
            serde_json::to_string(&action).unwrap(),
            r#""singleClickEdit""#
        );
    }

    #[test]
    fn zero_image_limit_means_unlimited() {
        let ry: Ry = serde_json::from_str(r#"{ "imageStorageLimitMb": 0 }"#).unwrap();

        assert_eq!(ry.image_storage_limit_bytes(), None);
    }
}
