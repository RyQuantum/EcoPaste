//! Alt 当 Ctrl 用（fork 补丁）：设置 `ry.windowModifier = "alt"` 时，剪贴板窗口可见期间
//! 导航钩子吞掉 Alt，改为注入 Ctrl（AutoHotkey `LAlt::LCtrl` 同款做法）。
//!
//! - 前台程序收不到 Alt，不会因为「单按一下 Alt」弹出菜单栏。
//! - 前端收到的是真正的 Ctrl：字母 / 符号 / 数字快捷键、提示徽标，以及「打开窗口时聚焦搜索框」
//!   时按 Ctrl 把焦点交还列表的逻辑，都和按 Ctrl 完全一样。Ctrl 本身照常可用。
//! - 窗口弹出时 Alt 已经按着（例如用 Win+Alt+C 唤出），那次 Alt 按下系统已经收到，吞不回来：
//!   这时按住的 Alt 只在钩子里算作 Ctrl，松开前补一次 Ctrl 按下松开，打断前台程序的菜单判定。
//! - 隐藏窗口时如果注入的 Ctrl 还没松开，立即补发松开，避免 Ctrl 卡住。

use std::mem::{size_of, zeroed};
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};
use winapi::shared::minwindef::UINT;
use winapi::um::winuser::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_KEYBOARD, KBDLLHOOKSTRUCT, KEYBDINPUT,
    KEYEVENTF_KEYUP, LLKHF_INJECTED, VK_LCONTROL, VK_LMENU, VK_MENU, VK_RMENU, WM_KEYDOWN,
    WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use super::NAV_EVENT;
use crate::settings::{SettingsStore, WindowModifier};

/// 当前是否有一次被吞掉的 Alt 按下、并已注入了 Ctrl 按下。
static CTRL_INJECTED: AtomicBool = AtomicBool::new(false);

/// 钩子对 Alt 键的处理结论。
pub enum AltKey {
    /// 不是要接管的 Alt（设置没选 Alt、不是 Alt 键、或是程序注入的 Alt），按原逻辑处理。
    NotMapped,
    /// 吞掉这个 Alt 事件。
    Swallow,
    /// 交给系统（窗口弹出前就按着的那次 Alt）。
    Pass,
}

/// Alt 是否也算窗口快捷键修饰键。
fn alt_mode(app: &AppHandle) -> bool {
    app.try_state::<SettingsStore>()
        .is_some_and(|store| store.snapshot().ry.window_modifier == WindowModifier::Alt)
}

/// 系统层面 Alt 是否按着。被钩子吞掉的 Alt 不会更新这个状态，所以为真只可能是
/// 窗口弹出前就按下、没被吞掉的那次。
fn alt_held_in_system() -> bool {
    (unsafe { GetAsyncKeyState(VK_MENU) } as u16) & 0x8000 != 0
}

fn is_alt(vk: u32) -> bool {
    matches!(vk as i32, VK_MENU | VK_LMENU | VK_RMENU)
}

/// 在导航钩子最前面调用，处理 Alt 键本身。
pub fn handle_alt_key(app: &AppHandle, kbd: &KBDLLHOOKSTRUCT, msg: UINT) -> AltKey {
    let injected = kbd.flags & LLKHF_INJECTED != 0;
    if injected || !is_alt(kbd.vkCode) || !alt_mode(app) {
        return AltKey::NotMapped;
    }

    if msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN {
        if CTRL_INJECTED.load(Ordering::Relaxed) {
            return AltKey::Swallow;
        }
        if alt_held_in_system() {
            return AltKey::Pass;
        }

        CTRL_INJECTED.store(true, Ordering::Relaxed);
        send_ctrl(&[0]);
        return AltKey::Swallow;
    }

    if msg == WM_KEYUP || msg == WM_SYSKEYUP {
        if CTRL_INJECTED.swap(false, Ordering::Relaxed) {
            send_ctrl(&[KEYEVENTF_KEYUP]);
            return AltKey::Swallow;
        }

        send_ctrl(&[0, KEYEVENTF_KEYUP]);
        return AltKey::Pass;
    }

    AltKey::NotMapped
}

/// 窗口弹出前就按着的 Alt：钩子里算作 Ctrl。
pub fn held_alt_acts_as_ctrl(app: &AppHandle) -> bool {
    alt_held_in_system() && alt_mode(app)
}

/// 窗口弹出时 Alt 已经按着，钩子收不到那次按下，主动补一个 Control 按下，让提示徽标立刻出现。
pub fn sync_held_alt(app: &AppHandle) {
    if !held_alt_acts_as_ctrl(app) {
        return;
    }

    if let Err(err) = app.emit(
        NAV_EVENT,
        json!({ "type": "keydown", "key": "Control", "ctrlKey": true }),
    ) {
        log::warn!("emit nav event failed: {err:?}");
    }
}

/// 隐藏窗口时调用：注入的 Ctrl 还按着就补发松开。
pub fn release() {
    if CTRL_INJECTED.swap(false, Ordering::Relaxed) {
        send_ctrl(&[KEYEVENTF_KEYUP]);
    }
}

fn send_ctrl(flags: &[u32]) {
    let mut inputs: Vec<INPUT> = flags
        .iter()
        .map(|flags| {
            let mut input: INPUT = unsafe { zeroed() };
            input.type_ = INPUT_KEYBOARD;
            unsafe {
                *input.u.ki_mut() = KEYBDINPUT {
                    wVk: VK_LCONTROL as u16,
                    wScan: 0,
                    dwFlags: *flags,
                    time: 0,
                    dwExtraInfo: 0,
                };
            }
            input
        })
        .collect();

    let sent = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_mut_ptr(),
            size_of::<INPUT>() as i32,
        )
    };
    if sent as usize != inputs.len() {
        log::warn!("inject ctrl for alt sent {sent}/{} events", inputs.len());
    }
}
