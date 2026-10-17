//! 中键「打开」文字（fork 补丁）：把文字写进临时文件，用设置里选的编辑器打开。
//!
//! 临时文件放在系统临时目录下的 `EcoPaste-open\`，文件名带时间。每次打开时顺手删掉
//! 一天前的旧文件；系统的存储感知 / 磁盘清理也会清系统临时目录。要留下内容，在编辑器里
//! 另存为或复制即可；关掉不存，这些文件最多留一天。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use crate::settings::OpenTextApp;

const TEMP_DIR_NAME: &str = "EcoPaste-open";
const KEEP_FOR: Duration = Duration::from_secs(24 * 60 * 60);

/// 写临时文件并用 `app` 打开；`Default` 交给 `default_opener`（系统给 .txt 关联的程序）。
/// 返回临时文件路径。
pub fn open_text(
    text: &str,
    app: OpenTextApp,
    default_opener: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join(TEMP_DIR_NAME);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{dir:?}: {e}"))?;
    remove_older_than(&dir, KEEP_FOR, SystemTime::now());
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let path = unique_path(&dir, &stamp);
    std::fs::write(&path, text).map_err(|e| format!("{path:?}: {e}"))?;
    match app {
        OpenTextApp::Default => default_opener(&path)?,
        _ => spawn(&launch_command(app, &path)?)?,
    }
    Ok(path)
}

/// 一条要启动的命令：程序 + 参数，以及是否需要新开控制台窗口（终端里的编辑器）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Launch {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub new_console: bool,
}

pub(crate) fn launch_command(app: OpenTextApp, file: &Path) -> Result<Launch, String> {
    let file_arg = file.to_string_lossy().into_owned();
    Ok(match app {
        OpenTextApp::Notepad => Launch {
            program: "notepad.exe".into(),
            args: vec![file_arg],
            new_console: false,
        },
        OpenTextApp::Vscode => Launch {
            program: find_vscode().ok_or("VS Code is not installed")?,
            args: vec![file_arg],
            new_console: false,
        },
        // 终端程序：有 Windows Terminal 就开在它的新窗口里，没有就让系统新开一个控制台。
        OpenTextApp::Neovim => match find_on_path("wt.exe") {
            Some(wt) => Launch {
                program: wt,
                args: vec!["nvim".into(), file_arg],
                new_console: false,
            },
            None => Launch {
                program: find_on_path("nvim.exe").ok_or("Neovim is not installed")?,
                args: vec![file_arg],
                new_console: true,
            },
        },
        OpenTextApp::Default => {
            return Err("default app is opened by the caller".into());
        }
    })
}

fn spawn(launch: &Launch) -> Result<(), String> {
    let mut cmd = Command::new(&launch.program);
    cmd.args(&launch.args);
    // 不让编辑器继承我们的标准输入输出：它活得比这次调用久，继承的句柄会一直被占着。
    // 新开控制台的终端程序例外，它要用自己的控制台。
    if !launch.new_console {
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        cmd.creation_flags(if launch.new_console {
            CREATE_NEW_CONSOLE
        } else {
            DETACHED_PROCESS
        });
    }
    cmd.spawn()
        .map(|_| ())
        .map_err(|e| format!("start {:?}: {e}", launch.program))
}

/// VS Code 的主程序（直接启动它，不经 `code.cmd`，免得闪一个控制台窗口）。
fn find_vscode() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        candidates.push(PathBuf::from(local).join(r"Programs\Microsoft VS Code\Code.exe"));
    }
    for var in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(dir) = std::env::var_os(var) {
            candidates.push(PathBuf::from(dir).join(r"Microsoft VS Code\Code.exe"));
        }
    }
    // PATH 里的 `bin\code.cmd` 旁边上一级就是 Code.exe。
    if let Some(cmd) = find_on_path("code.cmd") {
        if let Some(root) = cmd.parent().and_then(Path::parent) {
            candidates.push(root.join("Code.exe"));
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// 在 PATH 里找程序。用 `symlink_metadata`：`wt.exe` 这类应用执行别名是重解析点，
/// `metadata` / `is_file` 跟不进去会误判为不存在。
fn find_on_path(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(name))
        .find(|p| std::fs::symlink_metadata(p).is_ok())
}

/// `dir\EcoPaste-<stamp>.txt`，同一秒打开多次时加序号。
fn unique_path(dir: &Path, stamp: &str) -> PathBuf {
    let first = dir.join(format!("EcoPaste-{stamp}.txt"));
    if !first.exists() {
        return first;
    }
    (2..)
        .map(|n| dir.join(format!("EcoPaste-{stamp}-{n}.txt")))
        .find(|p| !p.exists())
        .expect("some suffix is free")
}

/// 删掉 `dir` 里修改时间早于 `now - keep` 的文件；删不掉的（还被占用）下次再试。
fn remove_older_than(dir: &Path, keep: Duration, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let old = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age > keep);
        if meta.is_file() && old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_temp_files_are_removed_and_recent_ones_kept() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old.txt");
        let fresh = dir.path().join("fresh.txt");
        std::fs::write(&old, "x").unwrap();
        std::fs::write(&fresh, "y").unwrap();
        let two_days_ago = SystemTime::now() - Duration::from_secs(2 * 24 * 60 * 60);
        std::fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(two_days_ago)
            .unwrap();

        remove_older_than(dir.path(), KEEP_FOR, SystemTime::now());

        assert!(!old.exists());
        assert!(fresh.exists());
    }

    #[test]
    fn same_second_opens_get_distinct_files() {
        let dir = tempfile::tempdir().unwrap();
        let a = unique_path(dir.path(), "20261017-010203");
        std::fs::write(&a, "a").unwrap();
        let b = unique_path(dir.path(), "20261017-010203");
        assert_ne!(a, b);
        assert!(b.ends_with("EcoPaste-20261017-010203-2.txt"));
    }

    #[test]
    fn notepad_gets_the_file_as_its_only_argument() {
        let launch = launch_command(OpenTextApp::Notepad, Path::new(r"C:\t\a b.txt")).unwrap();
        assert_eq!(launch.program, PathBuf::from("notepad.exe"));
        assert_eq!(launch.args, vec![r"C:\t\a b.txt".to_string()]);
        assert!(!launch.new_console);
    }

    /// 真的会打开编辑器窗口：`ECOPASTE_OPEN_TEXT_APP=neovim cargo test -- --ignored opens_text_in`。
    #[test]
    #[ignore = "opens an editor window on the desktop"]
    fn opens_text_in_the_chosen_editor() {
        let app = match std::env::var("ECOPASTE_OPEN_TEXT_APP").as_deref() {
            Ok("vscode") => OpenTextApp::Vscode,
            Ok("neovim") => OpenTextApp::Neovim,
            _ => OpenTextApp::Notepad,
        };
        let path = open_text(
            "EcoPaste 打开测试 ✅
second line",
            app,
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "EcoPaste 打开测试 ✅
second line"
        );
    }

    #[test]
    fn neovim_runs_inside_a_terminal() {
        let Ok(launch) = launch_command(OpenTextApp::Neovim, Path::new(r"C:\t\a.txt")) else {
            return; // Neovim not installed on this machine
        };
        let in_wt = launch.program.ends_with("wt.exe") && launch.args[0] == "nvim";
        assert!(in_wt || launch.new_console, "{launch:?}");
    }
}
