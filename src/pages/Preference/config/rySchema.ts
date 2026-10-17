import type { Settings } from "@/types/settings";
import { isWin } from "@/utils/is";
import type { PreferenceSetting } from "../types/preferences";

/**
 * RyQuantum/EcoPaste fork 专属设置项，按所属分区导出后在 `preferenceSchema.ts` 里展开，
 * 方便叠到上游新版上。设置值都在 `settings.ry` 段。
 */

/** 「快捷键」分区：窗口内快捷键修饰键。Alt 当 Ctrl 用由 Windows 导航钩子实现，macOS 不展示。 */
export const RY_SHORTCUT_SETTINGS: PreferenceSetting[] = isWin
  ? [
      {
        control: {
          options: [{ value: "primary" }, { value: "alt" }],
          type: "segmented",
        },
        id: "shortcuts.windowModifier",
        keywords: ["alt", "ctrl", "modifier", "number", "digit", "hint"],
        path: ["ry", "windowModifier"],
        value: (settings: Settings) => {
          return settings.ry.windowModifier;
        },
      },
    ]
  : [];

/** 「复制反馈」分区：拼接复制。键盘钩子目前只有 Windows 实现，macOS 不展示。 */
export const RY_COPY_SETTINGS: PreferenceSetting[] = isWin
  ? [
      {
        control: { type: "switch" },
        id: "copy.mergeCopy",
        keywords: ["merge", "append", "concat", "copy", "ctrl+c"],
        path: ["ry", "mergeCopy", "enabled"],
        value: (settings: Settings) => {
          return settings.ry.mergeCopy.enabled;
        },
      },
      {
        control: {
          options: [{ value: "space" }, { value: "newline" }],
          type: "segmented",
        },
        disabledWhen: (settings: Settings) => {
          return !settings.ry.mergeCopy.enabled;
        },
        id: "copy.mergeSeparator",
        keywords: ["merge", "separator", "space", "newline"],
        parentId: "copy.mergeCopy",
        path: ["ry", "mergeCopy", "separator"],
        value: (settings: Settings) => {
          return settings.ry.mergeCopy.separator;
        },
      },
      {
        control: { type: "switch" },
        disabledWhen: (settings: Settings) => {
          return !settings.ry.mergeCopy.enabled;
        },
        id: "copy.mergeSound",
        keywords: ["merge", "sound", "feedback"],
        parentId: "copy.mergeCopy",
        path: ["ry", "mergeCopy", "sound"],
        value: (settings: Settings) => {
          return settings.ry.mergeCopy.sound;
        },
      },
      {
        control: { type: "switch" },
        id: "copy.lanSync",
        keywords: ["sync", "lan", "crosspaste", "mac", "device", "network"],
        path: ["ry", "lanSync", "enabled"],
        value: (settings: Settings) => {
          return settings.ry.lanSync.enabled;
        },
      },
      {
        control: {
          options: [{ value: "off" }, { value: "images" }, { value: "all" }],
          type: "segmented",
        },
        disabledWhen: (settings: Settings) => {
          return !settings.ry.lanSync.enabled;
        },
        id: "copy.lanSyncSound",
        keywords: ["sync", "sound", "receive", "crosspaste"],
        parentId: "copy.lanSync",
        path: ["ry", "lanSync", "sound"],
        value: (settings: Settings) => {
          return settings.ry.lanSync.sound;
        },
      },
    ]
  : [];

/** 「粘贴行为」分区：中键「打开」文字时用的编辑器（图片固定用截图工具）。只在 Windows 显示。 */
export const RY_PASTE_SETTINGS: PreferenceSetting[] = isWin
  ? [
      {
        control: {
          options: [
            { value: "notepad" },
            { value: "vscode" },
            { value: "neovim" },
            { value: "default" },
          ],
          type: "select",
        },
        disabledWhen: (settings: Settings) => {
          return settings.clipboard.content.middleClick !== "singleClickOpen";
        },
        id: "paste.openTextWith",
        keywords: ["open", "editor", "notepad", "vscode", "neovim", "middle"],
        parentId: "paste.middleClick",
        path: ["ry", "openTextWith"],
        value: (settings: Settings) => {
          return settings.ry.openTextWith;
        },
      },
    ]
  : [];

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
