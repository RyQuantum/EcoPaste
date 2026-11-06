import { settingsState } from "@/stores/settings";

/**
 * Alt 当 Ctrl 用（fork 补丁，设置 `ry.windowModifier` 为 `alt` 或 `altOnly`）的浏览器侧补丁。
 *
 * 剪贴板窗口里输入框聚焦时 Rust 导航钩子是关着的，按键直接进 WebView：只有 Ctrl 会把焦点
 * 从搜索框交还列表。这里把按着 Alt 的浏览器键盘事件改写成按着 Ctrl 的同名事件，
 * 交还焦点、提示徽标和快捷键都走原有的 Ctrl 逻辑。钩子开着时 Alt 已在 Rust 侧换成 Ctrl，不经过这里。
 */
export function withAltAsCtrl(event: KeyboardEvent): KeyboardEvent {
  const modifier = settingsState.ry?.windowModifier;
  if (modifier !== "alt" && modifier !== "altOnly") return event;
  if (!event.altKey && event.key !== "Alt") return event;
  if (event.ctrlKey) return event;

  event.preventDefault();

  const isAltKey = event.key === "Alt";

  return new KeyboardEvent(event.type, {
    cancelable: true,
    code: isAltKey ? "ControlLeft" : event.code,
    ctrlKey: isAltKey ? event.type === "keydown" : true,
    key: isAltKey ? "Control" : event.key,
    metaKey: event.metaKey,
    repeat: event.repeat,
    shiftKey: event.shiftKey,
  });
}

/**
 * 「只用 Alt」档下的物理 Ctrl 事件（Ctrl 键本身，或按着 Ctrl、没按 Alt 的组合）：
 * 不算窗口快捷键，原样留给输入框做文本编辑。
 */
export function isPhysicalCtrlForEditing(event: KeyboardEvent) {
  if (settingsState.ry?.windowModifier !== "altOnly") return false;
  if (event.altKey) return false;

  return event.ctrlKey || event.key === "Control";
}
