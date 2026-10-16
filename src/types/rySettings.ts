/**
 * RyQuantum/EcoPaste fork 专属设置，镜像 `src-tauri/src/settings/ry.rs::Ry`。
 * 集中在 `ry` 段，方便叠到上游新版上；Rust 端改字段时本文件必须同步。
 */

export interface RySettings {
  /** 图片原图总占用上限，单位 MB；`0` = 不限。 */
  imageStorageLimitMb: number;
}
