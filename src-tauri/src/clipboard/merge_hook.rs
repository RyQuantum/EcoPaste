//! 拼接复制的 Windows 低级键盘钩子（fork 补丁）：只观察 Ctrl / C 的按下松开，
//! 喂给 `merge.rs` 的状态机；所有按键都原样交给下一个钩子，前台程序照常收到 Ctrl+C。
//!
//! 与 `keyboard/`（剪贴板窗口可见时才装、会吞导航键）不同，本钩子常驻且从不吞键。
//! 钩子回调里只做加锁改状态，不做 IO，避免拖慢系统输入（超时的低级钩子会被系统摘掉）。
//! 鼠标经 Deskflow 切到别的电脑时不计数：那边的 Ctrl+C 与本机剪贴板无关。

use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, Ordering};

use winapi::shared::minwindef::{LPARAM, LRESULT, UINT, WPARAM};
use winapi::um::winuser::{
    CallNextHookEx, GetAsyncKeyState, GetMessageW, SetWindowsHookExW, KBDLLHOOKSTRUCT, MSG,
    VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU, VK_RCONTROL, VK_RMENU,
    VK_RSHIFT, VK_RWIN, VK_SHIFT, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use super::merge;
use crate::keyboard::deskflow;

const VK_C: i32 = 0x43;

static STARTED: AtomicBool = AtomicBool::new(false);
static WAS_LOCAL: AtomicBool = AtomicBool::new(true);

/// 起一个常驻线程装钩子并跑消息泵；重复调用无副作用。
pub fn start() {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }

    deskflow::start();

    let spawned = std::thread::Builder::new()
        .name("merge-copy-hook".to_owned())
        .spawn(|| unsafe {
            let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), null_mut(), 0);
            if hook.is_null() {
                log::error!("merge copy: SetWindowsHookExW failed, merge copy is unavailable");
                return;
            }
            log::info!("merge copy keyboard hook installed");

            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {}
        });

    if let Err(err) = spawned {
        log::error!("merge copy: spawn hook thread failed: {err}");
    }
}

fn is_down(vk: i32) -> bool {
    (unsafe { GetAsyncKeyState(vk) } as u16) & 0x8000 != 0
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let local = deskflow::is_local();
        // 刚切走：按 Ctrl 已松开收尾，暂扣的条目照常入库。
        if WAS_LOCAL.swap(local, Ordering::Relaxed) && !local {
            merge::on_ctrl(false);
        }

        if local {
            let kbd = &*(lparam as *const KBDLLHOOKSTRUCT);
            handle_key(kbd.vkCode as i32, wparam as UINT);
        }
    }

    CallNextHookEx(null_mut(), code, wparam, lparam)
}

fn handle_key(vk: i32, msg: UINT) {
    let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
    let up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
    if !down && !up {
        return;
    }

    match vk {
        VK_CONTROL | VK_LCONTROL | VK_RCONTROL => merge::on_ctrl(down),
        VK_SHIFT | VK_LSHIFT | VK_RSHIFT | VK_MENU | VK_LMENU | VK_RMENU | VK_LWIN | VK_RWIN => {}
        VK_C if up => merge::on_c(false),
        VK_C => {
            // 只认「只按着 Ctrl」的 C：Ctrl+Shift+C（开发者工具、终端复制）等组合打断连按。
            let only_ctrl = is_down(VK_CONTROL)
                && !is_down(VK_SHIFT)
                && !is_down(VK_MENU)
                && !is_down(VK_LWIN)
                && !is_down(VK_RWIN);
            if only_ctrl {
                merge::on_c(true);
            } else {
                merge::on_other_key();
            }
        }
        _ if down && is_down(VK_CONTROL) => merge::on_other_key(),
        _ => {}
    }
}
