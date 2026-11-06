//! 把按键交还搜索框（fork 补丁，仅 Windows）。
//!
//! 剪贴板窗口默认不可聚焦，焦点在列表上时按键经导航钩子进来。有两类键本该给搜索框：
//! - ←/→（含 Shift、Ctrl 组合）：在搜索框里移动光标、选词。
//! - 设置选「只用 Alt」时，按着物理 Ctrl 的编辑组合（Ctrl+A/C/V/X/Z、Ctrl+Backspace 等）。
//!
//! 钩子吞掉这次按下，发 [`SEARCH_HANDOFF_EVENT`] 给前端；前端把焦点交回搜索框（窗口转为可聚焦、
//! 导航钩子随之关掉），再调 `replay_search_key` 让 [`replay`] 把同一个键重发一次，
//! 由输入框做原生的移动光标、选词、剪切复制粘贴。按住不放产生的自动重复按下在交还完成前只算一次。

use std::mem::{size_of, zeroed};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use winapi::um::winuser::{
    GetAsyncKeyState, MapVirtualKeyW, SendInput, INPUT, INPUT_KEYBOARD, KEYBDINPUT,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, MAPVK_VK_TO_VSC, VK_BACK, VK_CONTROL, VK_DELETE,
    VK_END, VK_HOME, VK_INSERT, VK_LCONTROL, VK_LEFT, VK_LSHIFT, VK_RIGHT, VK_SHIFT,
};

pub const SEARCH_HANDOFF_EVENT: &str = "keyboard://search-handoff";

/// 要交还搜索框的一次按键，以及吞键那一刻 Ctrl / Shift 是否按着。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HandoffKey {
    pub vk: u32,
    pub ctrl: bool,
    pub shift: bool,
}

/// ←/→：任何修饰键状态下都交还搜索框。
pub fn is_horizontal(vk: u32) -> bool {
    matches!(vk as i32, VK_LEFT | VK_RIGHT)
}

/// 「只用 Alt」档下，按着物理 Ctrl 时交还搜索框的键：字母、数字、标点和编辑键。
/// 方向键上下、回车、Esc、Tab、空格、F 键仍按导航键处理，不在此列。
pub fn is_ctrl_edit_key(vk: u32) -> bool {
    matches!(
        vk as i32,
        0x30..=0x39 // 0-9
            | 0x41..=0x5A // A-Z
            | 0xBA..=0xC0 // ; = , - . / `
            | 0xDB..=0xDF // [ \ ] '
            | VK_BACK
            | VK_DELETE
            | VK_INSERT
            | VK_HOME
            | VK_END
    ) || is_horizontal(vk)
}

fn is_down(vk: i32) -> bool {
    (unsafe { GetAsyncKeyState(vk) } as u16) & 0x8000 != 0
}

/// 记下当前 Ctrl / Shift 状态，通知前端交还搜索框。
pub fn emit(app: &AppHandle, vk: u32) {
    let key = HandoffKey {
        vk,
        ctrl: is_down(VK_CONTROL),
        shift: is_down(VK_SHIFT),
    };
    if let Err(err) = app.emit(SEARCH_HANDOFF_EVENT, key) {
        log::warn!("emit search handoff failed: {err:?}");
    }
}

/// 把吞掉的键重发一次。吞键时按着、现在已松开的 Ctrl / Shift 临时补按，发完再松开，
/// 免得快速点按的 Ctrl+V 在重发时变成单独的 V。
pub fn replay(key: HandoffKey) {
    let events = replay_events(key, is_down(VK_CONTROL), is_down(VK_SHIFT));
    let mut inputs: Vec<INPUT> = events.iter().map(|(vk, up)| key_input(*vk, *up)).collect();

    let sent = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_mut_ptr(),
            size_of::<INPUT>() as i32,
        )
    };
    if sent as usize != inputs.len() {
        log::warn!("replay search key sent {sent}/{} events", inputs.len());
    }
}

/// 重发的按键序列：`(虚拟键, 是否松开)`。
fn replay_events(key: HandoffKey, ctrl_now: bool, shift_now: bool) -> Vec<(u16, bool)> {
    let mut press: Vec<u16> = Vec::new();
    if key.ctrl && !ctrl_now {
        press.push(VK_LCONTROL as u16);
    }
    if key.shift && !shift_now {
        press.push(VK_LSHIFT as u16);
    }

    let mut events: Vec<(u16, bool)> = press.iter().map(|vk| (*vk, false)).collect();
    events.push((key.vk as u16, false));
    events.push((key.vk as u16, true));
    events.extend(press.iter().rev().map(|vk| (*vk, true)));
    events
}

/// 方向键、Home/End、Insert/Delete 属于扩展键，不带标志会被当成小键盘上的同位键。
fn is_extended(vk: u16) -> bool {
    matches!(
        vk as i32,
        VK_LEFT | VK_RIGHT | VK_HOME | VK_END | VK_INSERT | VK_DELETE
    )
}

fn key_input(vk: u16, up: bool) -> INPUT {
    let mut flags = 0;
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    if is_extended(vk) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }

    let mut input: INPUT = unsafe { zeroed() };
    input.type_ = INPUT_KEYBOARD;
    unsafe {
        *input.u.ki_mut() = KEYBDINPUT {
            wVk: vk,
            wScan: MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC) as u16,
            dwFlags: flags,
            time: 0,
            dwExtraInfo: 0,
        };
    }
    input
}

#[cfg(test)]
mod tests {
    use super::*;

    const VK_V: u32 = 0x56;

    #[test]
    fn edit_keys_cover_typing_keys_but_not_navigation() {
        for vk in [0x41, 0x5A, 0x30, VK_BACK as u32, VK_LEFT as u32, 0xBE] {
            assert!(is_ctrl_edit_key(vk), "{vk:#x} should be handed off");
        }
        // ↑ ↓ 回车 Esc Tab 空格 F1 Shift
        for vk in [0x26, 0x28, 0x0D, 0x1B, 0x09, 0x20, 0x70, 0x10] {
            assert!(!is_ctrl_edit_key(vk), "{vk:#x} should stay a nav key");
        }
    }

    #[test]
    fn replay_presses_released_modifiers_around_the_key() {
        let key = HandoffKey {
            vk: VK_V,
            ctrl: true,
            shift: false,
        };
        // Ctrl 已经松开：补按 Ctrl，发完松开。
        assert_eq!(
            replay_events(key, false, false),
            [
                (VK_LCONTROL as u16, false),
                (VK_V as u16, false),
                (VK_V as u16, true),
                (VK_LCONTROL as u16, true),
            ]
        );
        // Ctrl 还按着：只发这个键，不动 Ctrl。
        assert_eq!(
            replay_events(key, true, false),
            [(VK_V as u16, false), (VK_V as u16, true)]
        );
    }

    #[test]
    fn replay_does_not_add_modifiers_that_were_not_held() {
        let key = HandoffKey {
            vk: VK_LEFT as u32,
            ctrl: false,
            shift: true,
        };
        assert_eq!(
            replay_events(key, false, true),
            [(VK_LEFT as u16, false), (VK_LEFT as u16, true)]
        );
        assert!(is_extended(VK_LEFT as u16));
        assert!(!is_extended(VK_V as u16));
    }
}
