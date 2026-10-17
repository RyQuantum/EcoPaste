//! 跟踪 Deskflow（键鼠共享）把鼠标切到了哪台电脑（fork 补丁）。
//!
//! 本机的低级键盘钩子排在 Deskflow 的钩子前面：鼠标在别的电脑上时，按键本该经 Deskflow 发过去，
//! 却会先被剪贴板窗口的导航钩子吞掉、被「Alt 当 Ctrl」改掉。钩子先问 [`is_local`]，
//! 为假时原样放行。
//!
//! Deskflow 的进程间接口不广播切屏，只能读它的内核日志：服务端每次切屏都写一行
//! `INFO: leaving computer`（离开本机）或 `INFO: entering computer`（回到本机）。
//! 读不到日志（没装 Deskflow、日志级别低于 INFO）时一律当作在本机，即原来的行为。

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// 鼠标是否在本机。
static LOCAL: AtomicBool = AtomicBool::new(true);
static STARTED: AtomicBool = AtomicBool::new(false);

const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// 首次读取 / 落后太多时只看日志末尾这么多字节。
const TAIL_BYTES: u64 = 64 * 1024;
/// 日志持续读不到这么多轮才当作 Deskflow 已经不在（轮转时文件会短暂消失）。
const MISSING_POLLS_BEFORE_LOCAL: u32 = 20;

/// 鼠标是否在本机。钩子回调里调用，只读一个原子变量。
pub(crate) fn is_local() -> bool {
    LOCAL.load(Ordering::Relaxed)
}

/// 起后台线程跟踪 Deskflow 日志；重复调用无副作用。
pub(crate) fn start() {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }

    let Some(path) = log_path() else {
        return;
    };

    let spawned = std::thread::Builder::new()
        .name("deskflow-watch".to_owned())
        .spawn(move || watch(&path));
    if let Err(err) = spawned {
        log::error!("deskflow: spawn watcher thread failed: {err}");
    }
}

/// 守护进程模式下 Deskflow 内核的日志。
fn log_path() -> Option<PathBuf> {
    let program_data = std::env::var_os("ProgramData")?;
    Some(
        PathBuf::from(program_data)
            .join("Deskflow")
            .join("deskflow-daemon.log"),
    )
}

fn watch(path: &Path) {
    let mut tail = LogTail::default();
    let mut missing_polls = 0u32;

    loop {
        match tail.poll(path) {
            Ok(state) => {
                missing_polls = 0;
                if let Some(local) = state {
                    set_local(local);
                }
            }
            Err(_) => {
                missing_polls = missing_polls.saturating_add(1);
                if missing_polls == MISSING_POLLS_BEFORE_LOCAL {
                    tail = LogTail::default();
                    set_local(true);
                }
            }
        }

        std::thread::sleep(POLL_INTERVAL);
    }
}

fn set_local(local: bool) {
    if LOCAL.swap(local, Ordering::Relaxed) != local {
        log::info!(
            "deskflow: pointer {}",
            if local {
                "back on this computer"
            } else {
                "left for another computer"
            }
        );
    }
}

/// 增量读取日志，记住读到哪里以及末尾还没写完的半行。
#[derive(Default)]
struct LogTail {
    pos: Option<u64>,
    partial: String,
}

impl LogTail {
    /// 读新增内容，返回其中最后一条切屏记录给出的状态；没有新记录返回 `None`。
    fn poll(&mut self, path: &Path) -> std::io::Result<Option<bool>> {
        let mut file = File::open(path)?;
        let len = file.metadata()?.len();

        let mut skip_first_line = false;
        let start = match self.pos {
            Some(pos) if len < pos => {
                // 日志轮转了：从新文件开头读。
                self.partial.clear();
                0
            }
            Some(pos) if len == pos => return Ok(None),
            Some(pos) if len - pos <= TAIL_BYTES => pos,
            _ => {
                // 首次读取或落后太多：只看末尾，第一行可能是半截，丢掉。
                self.partial.clear();
                let start = len.saturating_sub(TAIL_BYTES);
                skip_first_line = start > 0;
                start
            }
        };

        file.seek(SeekFrom::Start(start))?;
        let mut bytes = Vec::with_capacity((len - start) as usize);
        file.by_ref().take(len - start).read_to_end(&mut bytes)?;
        self.pos = Some(start + bytes.len() as u64);

        let mut text = std::mem::take(&mut self.partial);
        text.push_str(&String::from_utf8_lossy(&bytes));

        let complete_len = text.rfind('\n').map_or(0, |i| i + 1);
        self.partial = text.split_off(complete_len);

        let complete = if skip_first_line {
            text.split_once('\n').map_or("", |(_, rest)| rest)
        } else {
            text.as_str()
        };

        Ok(last_state(complete))
    }
}

/// 一段日志里最后一条切屏记录给出的状态。
fn last_state(text: &str) -> Option<bool> {
    text.lines().rev().find_map(line_state)
}

/// 一行日志是否表明鼠标离开 / 回到了本机。内核启动、停止时鼠标都在本机。
fn line_state(line: &str) -> Option<bool> {
    let (_, message) = line.trim_end().split_once("] ")?;
    match message {
        "INFO: leaving computer" => Some(false),
        "INFO: entering computer"
        | "INFO: starting core"
        | "INFO: stopped server"
        | "INFO: core stopped successfully" => Some(true),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn line_state_reads_switch_lines() {
        assert_eq!(
            line_state("[2026-10-17T13:24:34.315] INFO: leaving computer"),
            Some(false)
        );
        assert_eq!(
            line_state("[2026-10-17T13:24:37.912] INFO: entering computer\r"),
            Some(true)
        );
        assert_eq!(
            line_state("[2026-10-17T13:24:37.912] INFO: starting core"),
            Some(true)
        );
        assert_eq!(
            line_state(
                "[2026-10-17T13:24:34.315] INFO: switch from \"Ryan-MiniPC\" to \"ryans-macbook-air\" at 1439,507"
            ),
            None
        );
        assert_eq!(
            line_state("[2026-10-17T13:24:34.315] DEBUG: leaving computer"),
            None
        );
        assert_eq!(line_state("leaving computer"), None);
    }

    #[test]
    fn last_state_takes_the_latest_switch() {
        let log = "[t] INFO: leaving computer\n[t] INFO: entering computer\n[t] INFO: leaving computer\n[t] INFO: local layouts: zh, en\n";
        assert_eq!(last_state(log), Some(false));
        assert_eq!(last_state("[t] INFO: accepted client connection\n"), None);
    }

    fn append(path: &Path, text: &str) {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        file.write_all(text.as_bytes()).unwrap();
    }

    #[test]
    fn tail_follows_appends_partial_lines_and_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deskflow-daemon.log");
        let mut tail = LogTail::default();

        assert!(tail.poll(&path).is_err());

        append(
            &path,
            "[t] INFO: switch from \"a\" to \"b\"\n[t] INFO: leaving computer\n",
        );
        assert_eq!(tail.poll(&path).unwrap(), Some(false));
        assert_eq!(tail.poll(&path).unwrap(), None);

        // 一行分两次写完：前半行不算数。
        append(&path, "[t] INFO: entering comp");
        assert_eq!(tail.poll(&path).unwrap(), None);
        append(&path, "uter\n");
        assert_eq!(tail.poll(&path).unwrap(), Some(true));

        // 轮转：文件变短，从头读新文件。
        std::fs::write(&path, "[t] INFO: leaving computer\n").unwrap();
        assert_eq!(tail.poll(&path).unwrap(), Some(false));
    }

    #[test]
    fn first_poll_only_reads_the_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deskflow-daemon.log");

        append(&path, "[t] INFO: leaving computer\n");
        let filler = format!("[t] VERBOSE: {}\n", "x".repeat(200));
        let mut text = String::new();
        while (text.len() as u64) < TAIL_BYTES * 2 {
            text.push_str(&filler);
        }
        append(&path, &text);

        // 末尾 64 KB 里没有切屏记录：不改变状态。
        let mut tail = LogTail::default();
        assert_eq!(tail.poll(&path).unwrap(), None);

        append(&path, "[t] INFO: entering computer\n");
        assert_eq!(tail.poll(&path).unwrap(), Some(true));
    }
}
