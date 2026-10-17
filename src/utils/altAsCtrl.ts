import { settingsState } from "@/stores/settings";

/**
 * Alt 当 Ctrl 用（fork 补丁，设置 `ry.windowModifier = "alt"`）的浏览器侧补丁。
 *
 * 剪贴板窗口里输入框聚焦时 Rust 导航钩子是关着的，按键直接进 WebView：只有 Ctrl 会把焦点
 * 从搜索框交还列表。这里把按着 Alt 的浏览器键盘事件改写成按着 Ctrl 的同名事件，
 * 交还焦点、提示徽标和快捷键都走原有的 Ctrl 逻辑。钩子开着时 Alt 已在 Rust 侧换成 Ctrl，不经过这里。
 */
export function withAltAsCtrl(event: KeyboardEvent): KeyboardEvent {
  if (settingsState.ry?.windowModifier !== "alt") return event;
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
