/**
 * RyQuantum/EcoPaste fork 专属设置，镜像 `src-tauri/src/settings/ry.rs::Ry`。
 * 集中在 `ry` 段，方便叠到上游新版上；Rust 端改字段时本文件必须同步。
 */

/** Rust enum `WindowModifier`：`primary` 为平台主修饰键（Windows Ctrl）；`alt` 表示 Alt 与 Ctrl 等效。 */
export type WindowModifier = "primary" | "alt";

/** Rust enum `MergeSeparator`。 */
export type MergeSeparator = "space" | "newline";

export interface MergeCopy {
  enabled: boolean;
  separator: MergeSeparator;
  sound: boolean;
}

/** 局域网同步（CrossPaste 协议）。`enabled` 改动后重启生效。 */
/** Rust enum `OpenTextApp`：中键「打开」文字时用的编辑器；`default` 为系统给 .txt 关联的程序。 */
export type OpenTextApp = "notepad" | "vscode" | "neovim" | "default";

/** Rust enum `ReceiveSound`：收到内容时何时响；`images` 只在收到图片时响。 */
export type ReceiveSound = "off" | "images" | "all";

export interface LanSync {
  enabled: boolean;
  sound: ReceiveSound;
}

export interface RySettings {
  windowModifier: WindowModifier;
  mergeCopy: MergeCopy;
  /** 图片原图总占用上限，单位 MB；`0` = 不限。 */
  imageStorageLimitMb: number;
  lanSync: LanSync;
  openTextWith: OpenTextApp;
}
