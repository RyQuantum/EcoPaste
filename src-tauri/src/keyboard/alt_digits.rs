//! 「Alt + 数字」快速粘贴（fork 补丁）：设置 `ry.quickPasteModifier = "alt"` 时，
//! 剪贴板窗口可见期间由导航钩子捕获 Alt + 数字转发给前端，并吞掉数字键。
//!
//! Alt 本身不吞：Alt+Tab 等系统组合照常可用。但前台程序（剪贴板窗口不抢焦点，前台仍是原程序）
//! 会在「Alt 按下又松开、中间没有别的键」时激活菜单栏，所以 Alt 按下和数字被吞时各注入一次
//! 无意义的 0xE8 键（AutoHotkey 的 menu mask 同款做法）打断这个判定。

use std::mem::{size_of, zeroed};
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};
use winapi::shared::minwindef::UINT;
use winapi::um::winuser::{
    SendInput, INPUT, INPUT_KEYBOARD, KBDLLHOOKSTRUCT, KEYBDINPUT, KEYEVENTF_KEYUP, LLKHF_ALTDOWN,
    LLKHF_INJECTED, VK_LMENU, VK_MENU, VK_RMENU, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use super::NAV_EVENT;
use crate::settings::{QuickPasteModifier, SettingsStore};

/// 未分配的虚拟键码，系统和常见程序都不处理它。
const VK_MENU_MASK: u16 = 0xE8;

/// 当前这次 Alt 按住期间是否已注入过 menu mask；Alt 自动重复不重复注入。
static ALT_MASKED: AtomicBool = AtomicBool::new(false);

/// 处理结果：`Swallow` 表示钩子应返回 1 吞掉这个键；`Pass` 表示交给后续逻辑。
pub enum AltDigitAction {
    Swallow,
    Pass,
}

/// 设置里是否选了 Alt 作为快速粘贴修饰键。
pub fn alt_mode(app: &AppHandle) -> bool {
    app.try_state::<SettingsStore>()
        .is_some_and(|store| store.snapshot().ry.quick_paste_modifier == QuickPasteModifier::Alt)
}

fn is_alt(vk: u32) -> bool {
    matches!(vk as i32, VK_MENU | VK_LMENU | VK_RMENU)
}

pub fn is_digit(vk: u32) -> bool {
    (0x30..=0x39).contains(&vk)
}

/// 在导航钩子里、Ctrl 处理之前调用。只处理 Alt 键本身和 Alt + 数字，其余一律 `Pass`。
/// 吞掉的数字键会记入 `consumed`，配对的 KEYUP 由调用方原有逻辑一并吞掉。
pub fn handle(
    app: &AppHandle,
    kbd: &KBDLLHOOKSTRUCT,
    msg: UINT,
    ctrl_down: bool,
    consume: impl FnOnce(u32),
) -> AltDigitAction {
    let vk = kbd.vkCode;
    let injected = kbd.flags & LLKHF_INJECTED != 0;
    if injected || !(is_alt(vk) || is_digit(vk)) || !alt_mode(app) {
        return AltDigitAction::Pass;
    }

    let key_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
    let key_up = msg == WM_KEYUP || msg == WM_SYSKEYUP;

    if is_alt(vk) {
        if key_down && !ALT_MASKED.swap(true, Ordering::Relaxed) {
            inject_menu_mask();
        }
        if key_up {
            ALT_MASKED.store(false, Ordering::Relaxed);
        }
        if key_down || key_up {
            emit(
                app,
                json!({
                    "type": if key_down { "keydown" } else { "keyup" },
                    "key": "Alt",
                    "altKey": key_down,
                }),
            );
        }
        return AltDigitAction::Pass;
    }

    let alt_down = kbd.flags & LLKHF_ALTDOWN != 0;
    if !key_down || !alt_down || ctrl_down {
        return AltDigitAction::Pass;
    }

    emit(
        app,
        json!({
            "type": "keydown",
            "key": ((vk as u8) as char).to_string(),
            "altKey": true,
        }),
    );
    inject_menu_mask();
    consume(vk);
    AltDigitAction::Swallow
}

/// 隐藏窗口时通知前端 Alt 已松开，避免残留「Alt 按下」的提示徽标。
pub fn emit_alt_released(app: &AppHandle) {
    ALT_MASKED.store(false, Ordering::Relaxed);
    emit(
        app,
        json!({ "type": "keyup", "key": "Alt", "altKey": false }),
    );
}

fn emit(app: &AppHandle, payload: serde_json::Value) {
    if let Err(err) = app.emit(NAV_EVENT, payload) {
        log::warn!("emit nav event failed: {err:?}");
    }
}

fn inject_menu_mask() {
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
