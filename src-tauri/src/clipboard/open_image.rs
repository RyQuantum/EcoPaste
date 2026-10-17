//! 右键菜单「打开」图片（fork 补丁）：Windows 上交给截图工具（Snipping Tool）打开，可以直接标注；
//! 截图工具没装或拉不起来时、以及 macOS 上，用系统默认的看图程序打开。
//!
//! Snipping Tool 在清单里登记了 png / jpg 等图片的文件关联，所以走资源管理器「打开方式」
//! 同一套接口（`SHAssocEnumHandlers` → `IAssocHandler::Invoke`）把文件交给它。

use std::path::Path;

/// 打开图片文件。返回实际用了哪种方式，供日志使用。
pub fn open(
    path: &Path,
    opener: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<&'static str, String> {
    #[cfg(target_os = "windows")]
    {
        match open_in_snipping_tool(path) {
            Ok(()) => return Ok("snipping tool"),
            Err(err) => log::warn!("open image in Snipping Tool failed, using default app: {err}"),
        }
    }
    opener(path).map(|()| "default app")
}

/// 资源管理器「打开方式」同款：列出能打开这种扩展名的程序，挑出截图工具，让它打开文件。
/// 截图工具是打包的桌面程序，`IApplicationActivationManager::ActivateForFile` 对它报
/// 「不支持该协定」，命令行别名也不保证会加载传入的文件。
#[cfg(target_os = "windows")]
fn open_in_snipping_tool(path: &Path) -> windows::core::Result<()> {
    with_com(|| unsafe {
        let handler = find_handler(path, is_snipping_tool)?.ok_or_else(|| {
            windows::core::Error::new(
                windows::Win32::Foundation::E_FAIL,
                "Snipping Tool is not registered for this file type".into(),
            )
        })?;
        let data = data_object(path)?;
        handler.Invoke(&data)
    })
}

#[cfg(target_os = "windows")]
fn is_snipping_tool(name: &str, ui_name: &str) -> bool {
    name.to_ascii_lowercase().contains("microsoft.screensketch")
        || ui_name.to_ascii_lowercase().contains("snipping tool")
        || ui_name.contains("截图工具")
}

#[cfg(target_os = "windows")]
fn with_com<T>(f: impl FnOnce() -> windows::core::Result<T>) -> windows::core::Result<T> {
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
    unsafe {
        // 在调用方的阻塞线程上初始化 COM；已初始化过（S_FALSE）也要配对 Uninitialize。
        let initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let result = f();
        if initialized {
            CoUninitialize();
        }
        result
    }
}

/// 能打开 `path` 这种扩展名的程序里，第一个满足 `pick(name, ui_name)` 的。
#[cfg(target_os = "windows")]
unsafe fn find_handler(
    path: &Path,
    pick: impl Fn(&str, &str) -> bool,
) -> windows::core::Result<Option<windows::Win32::UI::Shell::IAssocHandler>> {
    use windows::core::HSTRING;
    use windows::Win32::UI::Shell::{SHAssocEnumHandlers, ASSOC_FILTER_NONE};

    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let handlers = SHAssocEnumHandlers(&HSTRING::from(ext), ASSOC_FILTER_NONE)?;
    loop {
        let mut slot = [None];
        let mut fetched = 0u32;
        handlers.Next(&mut slot, Some(&mut fetched))?;
        let Some(handler) = slot[0].take().filter(|_| fetched > 0) else {
            return Ok(None);
        };
        let name = take_string(handler.GetName());
        let ui_name = take_string(handler.GetUIName());
        if pick(&name, &ui_name) {
            return Ok(Some(handler));
        }
    }
}

#[cfg(target_os = "windows")]
unsafe fn take_string(s: windows::core::Result<windows::core::PWSTR>) -> String {
    let Ok(p) = s else {
        return String::new();
    };
    let text = p.to_string().unwrap_or_default();
    windows::Win32::System::Com::CoTaskMemFree(Some(p.as_ptr() as *const _));
    text
}

#[cfg(target_os = "windows")]
unsafe fn data_object(
    path: &Path,
) -> windows::core::Result<windows::Win32::System::Com::IDataObject> {
    use windows::core::HSTRING;
    use windows::Win32::UI::Shell::{BHID_DataObject, IShellItem, SHCreateItemFromParsingName};

    let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(path.as_os_str()), None)?;
    item.BindToHandler(None, &BHID_DataObject)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真的会弹出截图工具窗口，只在需要时手动跑：`--ignored opens_an_image_in_snipping_tool`。
    /// 列出本机能打开 .png 的程序（名字和显示名），排查截图工具没被选中时用。
    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "diagnostic: prints the registered .png handlers"]
    fn lists_png_handlers() {
        with_com(|| unsafe {
            find_handler(Path::new("x.png"), |name, ui| {
                println!("handler: {name} | {ui}");
                false
            })
        })
        .unwrap();
    }

    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "opens a Snipping Tool window on the desktop"]
    fn opens_an_image_in_snipping_tool() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("open-test.png");
        image::RgbImage::from_pixel(64, 48, image::Rgb([30, 144, 255]))
            .save(&path)
            .unwrap();
        open_in_snipping_tool(&path).unwrap();
        // 给截图工具时间读文件，再删临时目录。
        std::thread::sleep(std::time::Duration::from_secs(5));
    }

    #[test]
    fn falls_back_to_the_default_app_when_the_preferred_app_fails() {
        let path = Path::new("Z:\\definitely\\missing\\image.png");
        let mut used = None;
        let how = open(path, |p| {
            used = Some(p.to_path_buf());
            Ok(())
        })
        .unwrap();
        assert_eq!(how, "default app");
        assert_eq!(used.as_deref(), Some(path));
    }
}
