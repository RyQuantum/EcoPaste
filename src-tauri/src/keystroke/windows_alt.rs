//! 粘贴前松开物理按住的 Alt（fork 补丁）。
//!
//! 「Alt + 数字」快速粘贴时用户还按着 Alt，直接注入 Ctrl+V 会被前台程序当成 Ctrl+Alt+V
//! （Word 里是选择性粘贴，部分键盘布局下是 AltGr+V）。注入 Ctrl+V 之前先补一个 Alt 松开；
//! 前面先注入一次 0xE8 空键，避免这个 Alt 松开激活前台程序的菜单栏。
//! 之后用户真正松开 Alt 时系统里 Alt 已是松开状态，那次松开是空操作。

use std::mem::{size_of, zeroed};

use winapi::um::winuser::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_LMENU,
    VK_RMENU,
};

const VK_MENU_MASK: u16 = 0xE8;

/// 当前按着的 Alt 键（左 / 右各自注入松开）；都没按时什么也不做。
pub fn release_held_alt() {
    let held: Vec<u16> = [VK_LMENU, VK_RMENU]
        .into_iter()
        .filter(|vk| (unsafe { GetAsyncKeyState(*vk) } as u16) & 0x8000 != 0)
        .map(|vk| vk as u16)
        .collect();
    if held.is_empty() {
        return;
    }

    let mut events = vec![(VK_MENU_MASK, 0), (VK_MENU_MASK, KEYEVENTF_KEYUP)];
    events.extend(held.into_iter().map(|vk| (vk, KEYEVENTF_KEYUP)));

    let mut inputs: Vec<INPUT> = events
        .into_iter()
        .map(|(vk, flags)| {
            let mut input: INPUT = unsafe { zeroed() };
            input.type_ = INPUT_KEYBOARD;
            unsafe {
                *input.u.ki_mut() = KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: flags,
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
        log::warn!("release held alt sent {sent}/{} events", inputs.len());
    }
}
