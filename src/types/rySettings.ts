/**
 * RyQuantum/EcoPaste fork 专属设置，镜像 `src-tauri/src/settings/ry.rs::Ry`。
 * 集中在 `ry` 段，方便叠到上游新版上；Rust 端改字段时本文件必须同步。
 */

/** Rust enum `QuickPasteModifier`：`primary` 为平台主修饰键（macOS ⌘ / Windows Ctrl）。 */
export type QuickPasteModifier = "primary" | "alt";

/** Rust enum `MergeSeparator`。 */
export type MergeSeparator = "space" | "newline";

export interface MergeCopy {
  enabled: boolean;
  separator: MergeSeparator;
}

export interface RySettings {
  quickPasteModifier: QuickPasteModifier;
  mergeCopy: MergeCopy;
  /** 图片原图总占用上限，单位 MB；`0` = 不限。 */
  imageStorageLimitMb: number;
}
