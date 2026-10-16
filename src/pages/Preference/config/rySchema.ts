import type { PreferenceSetting } from "../types/preferences";

/**
 * RyQuantum/EcoPaste fork 专属设置项，按所属分区导出后在 `preferenceSchema.ts` 里展开，
 * 方便叠到上游新版上。设置值都在 `settings.ry` 段。
 */

/** 「保留与清理」分区：图片空间上限。 */
export const RY_HISTORY_SETTINGS: PreferenceSetting[] = [
  {
    control: { min: 0, suffixKey: "mb", type: "number" },
    id: "history.imageStorageLimitMb",
    keywords: ["image", "storage", "space", "disk", "limit", "mb", "cleanup"],
    path: ["ry", "imageStorageLimitMb"],
    value: (settings) => {
      return settings.ry.imageStorageLimitMb;
    },
  },
];
