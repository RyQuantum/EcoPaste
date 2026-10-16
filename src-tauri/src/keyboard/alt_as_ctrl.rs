//! Alt 当 Ctrl 用（fork 补丁）：设置 `ry.windowModifier = "alt"` 时，剪贴板窗口可见期间
//! 导航钩子把 Alt 视为 Ctrl——上报给前端的是 Control 事件，Alt + 字母 / 符号 / 数字和 Ctrl + 同键
//! 走同一条路径，提示徽标、快捷键全部一致，前端不需要知道有 Alt 这回事。Ctrl 本身照常可用。
//!
//! Alt 键本身不吞（Alt+Tab 等系统组合照常可用）。前台程序（剪贴板窗口不抢焦点，前台仍是原程序）
//! 会在「Alt 按下又松开、中间没有别的键」时激活菜单栏，所以每次按住 Alt 期间注入一次无意义的
//! 0xE8 键（AutoHotkey 的 menu mask 同款做法）打断这个判定。

use std::mem::{size_of, zeroed};
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};
use winapi::shared::minwindef::UINT;
use winapi::um::winuser::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_LMENU,
    VK_MENU, VK_RMENU, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use super::NAV_EVENT;
use crate::settings::{SettingsStore, WindowModifier};

/// 未分配的虚拟键码，系统和常见程序都不处理它。
const VK_MENU_MASK: u16 = 0xE8;

/// 本次按住 Alt 期间是否已注入过 menu mask。
static ALT_MASKED: AtomicBool = AtomicBool::new(false);

/// Alt 是否也算窗口快捷键修饰键。
fn alt_mode(app: &AppHandle) -> bool {
    app.try_state::<SettingsStore>()
        .is_some_and(|store| store.snapshot().ry.window_modifier == WindowModifier::Alt)
}

fn alt_held() -> bool {
    (unsafe { GetAsyncKeyState(VK_MENU) } as u16) & 0x8000 != 0
}

fn is_alt(vk: u32) -> bool {
    matches!(vk as i32, VK_MENU | VK_LMENU | VK_RMENU)
}

/// 当前是否「按着充当 Ctrl 的 Alt」。是的话顺带确保本次按住期间注入过 menu mask
/// （窗口在 Alt 已按下时才弹出，钩子看不到那次 Alt 按下，只能在这里补）。
pub fn alt_acts_as_ctrl(app: &AppHandle) -> bool {
    if !alt_held() || !alt_mode(app) {
        return false;
    }

    mask_menu_once();
    true
}

/// 这个键是否是充当 Ctrl 的 Alt 本身；是的话调用方按 Ctrl 键处理（上报 Control、不吞）。
pub fn is_mapped_alt(app: &AppHandle, vk: u32, msg: UINT) -> bool {
    if !is_alt(vk) || !alt_mode(app) {
        return false;
    }

    if msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN {
        mask_menu_once();
    } else if msg == WM_KEYUP || msg == WM_SYSKEYUP {
        ALT_MASKED.store(false, Ordering::Relaxed);
    }
    true
}

/// 窗口弹出时 Alt 已经按着（例如用 Win+Alt+C 唤出），钩子收不到那次按下，
/// 主动补一个 Control 按下，让提示徽标立刻出现。
pub fn sync_held_alt(app: &AppHandle) {
    if !alt_held() || !alt_mode(app) {
        return;
    }

    if let Err(err) = app.emit(
        NAV_EVENT,
        json!({ "type": "keydown", "key": "Control", "ctrlKey": true }),
    ) {
        log::warn!("emit nav event failed: {err:?}");
    }
}

/// 隐藏窗口时复位：下次按住 Alt 重新注入。
pub fn reset() {
    ALT_MASKED.store(false, Ordering::Relaxed);
}

fn mask_menu_once() {
    if ALT_MASKED.swap(true, Ordering::Relaxed) {
        return;
    }

    let mut inputs: [INPUT; 2] = unsafe { zeroed() };
    for (input, flags) in inputs.iter_mut().zip([0, KEYEVENTF_KEYUP]) {
        input.type_ = INPUT_KEYBOARD;
        unsafe {
            *input.u.ki_mut() = KEYBDINPUT {
                wVk: VK_MENU_MASK,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            };
        }
    }

    let sent = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_mut_ptr(),
            size_of::<INPUT>() as i32,
        )
    };
    if sent as usize != inputs.len() {
        log::warn!("inject menu mask key sent {sent}/{} events", inputs.len());
    }
}
