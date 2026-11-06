//! fork 补丁：把导航钩子吞掉的按键交还搜索框后重发（见 `keyboard/search_handoff.rs`）。

/// 前端已把焦点交回搜索框，重发那次被吞掉的按键。
#[cfg(target_os = "windows")]
#[tauri::command]
pub async fn replay_search_key(key: crate::keyboard::HandoffKey) {
    crate::keyboard::replay_search_key(key);
}

/// 只有 Windows 的剪贴板窗口会交还按键；其他平台不会调到这里。
#[cfg(not(target_os = "windows"))]
#[tauri::command]
pub async fn replay_search_key() {}
