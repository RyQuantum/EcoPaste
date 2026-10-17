//! 拼接复制（fork 补丁，仿 Alfred Merging）：按住 Ctrl 连按两下 C（间隔 ≤ 0.5 秒），
//! 把这次复制的文本接到上一条历史后面；中间片段不进历史，只保留拼好的结果。
//!
//! 数据流：
//! - 键盘：`merge_hook.rs`（仅 Windows）用低级键盘钩子把 Ctrl / C 的按下松开喂给 [`MergeTracker`]，
//!   按键本身照常传给前台程序。
//! - 剪贴板：watcher 把每次读到的新条目交给 [`dispatch`]。按住 Ctrl 刚按过一次 C 时，
//!   文本条目先扣住不入库（它可能是中间片段）；第二下 C 之后剪贴板真的变了，才拼接；
//!   Ctrl 松开或等不到第二下时，扣住的条目照常入库。
//!   终端里连按 Ctrl+C 中断程序不会改剪贴板，所以不会触发。
//!
//! 非 Windows 平台或设置里关闭时，[`dispatch`] 等价于直接入库。

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};

use super::guard::WritebackGuard;
use super::ingest::build_item_with_settings;
use super::payload::{ClipboardPayload, TextPayload};
use super::storage::ImageStore;
use super::watcher::{persist_and_notify, CLIPBOARD_UPDATED_EVENT};
use super::write::write_to_clipboard;
use crate::db::items::delete_item;
use crate::db::latest::find_latest_item;
use crate::db::models::{ClipboardApp, ClipboardItem, ClipboardKind, ClipboardSubKind};
use crate::settings::{MergeSeparator, SettingsStore};

/// 两下 C 之间允许的最长间隔。
const PRESS_WINDOW: Duration = Duration::from_millis(500);
/// 第二下 C 之后，等剪贴板变化的最长时间；超时视为这一下没有复制出东西。
const CAPTURE_WINDOW: Duration = Duration::from_millis(1000);
/// 定时器比截止时间多等一点，避免恰好卡在边界上又被判定为「还在等」。
const TIMER_SLACK: Duration = Duration::from_millis(20);

/// 剪贴板变化的处理结论。
#[derive(Debug, PartialEq, Eq)]
pub enum Decision<T> {
    /// 照常入库。
    Persist(T),
    /// 扣住，等到 `until` 还没有第二下 C 就入库。
    Hold { until: Instant },
    /// 第二下 C 之后的复制：接到上一条历史后面。
    Merge(T),
    /// 已拼接过的这一下 C 又触发了一次剪贴板变化（有些程序复制时会写两遍），丢弃。
    Drop,
}

/// 连按检测 + 中间片段暂存的纯状态机，时间由调用方传入，方便单测。
#[derive(Debug)]
pub struct MergeTracker<T> {
    ctrl_held: bool,
    c_down: bool,
    /// 每算一下 C 就加一，用来判断「拼接之后有没有再按过 C」。
    press_seq: u64,
    /// 本次按住 Ctrl 期间最近一下 C 的时间；Ctrl 松开或按了别的键就清空。
    last_press: Option<Instant>,
    /// 最近一下「算作连按」的 C 的时间，等它带来的剪贴板变化。
    armed_at: Option<Instant>,
    /// 已经拼接过的那一下 C（序号、拼接时间），用来丢弃同一下 C 的重复写入。
    merged: Option<(u64, Instant)>,
    /// 暂扣的文本条目（可能是中间片段）及它所属那一下 C 的时间。
    held: Option<(T, Instant)>,
}

impl<T> Default for MergeTracker<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> MergeTracker<T> {
    pub const fn new() -> Self {
        Self {
            ctrl_held: false,
            c_down: false,
            press_seq: 0,
            last_press: None,
            armed_at: None,
            merged: None,
            held: None,
        }
    }

    pub fn ctrl_down(&mut self) {
        self.ctrl_held = true;
    }

    /// Ctrl 松开：连按序列结束。没有在等第二下的剪贴板变化时，扣住的条目立即放行。
    pub fn ctrl_up(&mut self, now: Instant) -> Vec<T> {
        self.ctrl_held = false;
        self.last_press = None;
        self.expire(now)
    }

    /// C 按下。按住不放的自动重复不算新的一下。
    /// 这一下算作连按时，返回等剪贴板变化的截止时间，调用方到点要 [`Self::tick`] 一次。
    pub fn c_down(&mut self, now: Instant) -> Option<Instant> {
        if self.c_down {
            return None;
        }
        self.c_down = true;

        if !self.ctrl_held {
            self.last_press = None;
            return None;
        }

        self.press_seq += 1;
        let armed = self
            .last_press
            .is_some_and(|prev| now.saturating_duration_since(prev) <= PRESS_WINDOW);
        self.last_press = Some(now);

        if !armed {
            return None;
        }
        self.armed_at = Some(now);
        Some(now + CAPTURE_WINDOW)
    }

    pub fn c_up(&mut self) {
        self.c_down = false;
    }

    /// 按住 Ctrl 时按了 C 以外的键（如 Ctrl+A）：打断连按。
    pub fn other_key(&mut self) {
        self.last_press = None;
    }

    /// 剪贴板变化。返回需要先放行入库的条目，以及这次变化本身怎么处理。
    pub fn on_change(&mut self, now: Instant, value: T, is_text: bool) -> (Vec<T>, Decision<T>) {
        let mut released = self.expire(now);

        if self.armed_at.take().is_some() {
            if is_text {
                self.held = None;
                self.merged = Some((self.press_seq, now));
                return (released, Decision::Merge(value));
            }

            released.extend(self.take_held());
            return (released, Decision::Persist(value));
        }

        if is_text && self.merged.is_some_and(|(seq, _)| seq == self.press_seq) {
            return (released, Decision::Drop);
        }

        if let (true, Some(press)) = (is_text && self.sequence_open(now), self.last_press) {
            // expire 之后还扣着的只可能属于同一下 C：这下 C 写了两次剪贴板，只留最新的一份。
            self.held = Some((value, press));
            return (
                released,
                Decision::Hold {
                    until: press + PRESS_WINDOW,
                },
            );
        }

        released.extend(self.take_held());
        (released, Decision::Persist(value))
    }

    /// 到点检查：等第二下或等剪贴板变化超时后，扣住的条目放行。
    pub fn tick(&mut self, now: Instant) -> Vec<T> {
        self.expire(now)
    }

    fn sequence_open(&self, now: Instant) -> bool {
        self.ctrl_held
            && self
                .last_press
                .is_some_and(|press| now.saturating_duration_since(press) <= PRESS_WINDOW)
    }

    fn take_held(&mut self) -> Option<T> {
        self.held.take().map(|(value, _)| value)
    }

    /// 清掉过期状态，返回该放行的扣留条目。
    /// 还在等第二下 C 带来的变化时一律继续扣；否则只有「仍是最近一下 C、仍按着 Ctrl、没超时」才继续扣。
    fn expire(&mut self, now: Instant) -> Vec<T> {
        if self
            .armed_at
            .is_some_and(|armed| now.saturating_duration_since(armed) > CAPTURE_WINDOW)
        {
            self.armed_at = None;
        }

        if self
            .merged
            .is_some_and(|(_, at)| now.saturating_duration_since(at) > CAPTURE_WINDOW)
        {
            self.merged = None;
        }

        if self.armed_at.is_some() {
            return Vec::new();
        }

        let keep = self
            .held
            .as_ref()
            .is_some_and(|(_, press)| self.last_press == Some(*press) && self.sequence_open(now));
        if keep {
            return Vec::new();
        }

        self.take_held().into_iter().collect()
    }
}

/// 拼接结果：上一条 + 分隔符 + 新内容。
pub fn merge_text(previous: &str, addition: &str, separator: MergeSeparator) -> String {
    format!("{previous}{}{addition}", separator.as_str())
}

/// 文本条目的纯文本：HTML / RTF 条目的 `content` 是富文本源码，纯文本在 `search_text`。
pub fn plain_text(item: &ClipboardItem) -> Option<&str> {
    if item.kind != ClipboardKind::Text {
        return None;
    }

    match item.sub_kind {
        Some(ClipboardSubKind::Html | ClipboardSubKind::Rtf) => {
            Some(item.search_text.as_deref().unwrap_or(&item.content))
        }
        _ => Some(&item.content),
    }
}

/// 上一条带备注、分组、置顶或收藏时保留原条目，拼好的结果另起一条；否则用结果替换它。
pub fn keeps_previous(item: &ClipboardItem) -> bool {
    item.is_pinned || item.is_favorite || item.note.is_some() || item.group_id.is_some()
}

/// watcher 读到的一条待入库内容。
pub struct Pending {
    pub item: ClipboardItem,
    pub source_app: Option<ClipboardApp>,
}

static TRACKER: Mutex<MergeTracker<Pending>> = Mutex::new(MergeTracker::new());
static APP: OnceLock<AppHandle> = OnceLock::new();

/// 在 watcher 初始化时调用：记下 AppHandle，Windows 上装键盘钩子。
pub fn init(app: &AppHandle) {
    let _ = APP.set(app.clone());

    #[cfg(target_os = "windows")]
    super::merge_hook::start();
}

fn lock_tracker() -> std::sync::MutexGuard<'static, MergeTracker<Pending>> {
    TRACKER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 键盘钩子：Ctrl 按下 / 松开。松开时放行的条目交给异步运行时入库。
#[cfg(target_os = "windows")]
pub(super) fn on_ctrl(down: bool) {
    if down {
        lock_tracker().ctrl_down();
        return;
    }

    let released = lock_tracker().ctrl_up(Instant::now());
    if let Some(app) = APP.get() {
        persist_all(app.clone(), released);
    }
}

/// 键盘钩子：C 按下 / 松开（只在只按着 Ctrl 时上报按下）。
#[cfg(target_os = "windows")]
pub(super) fn on_c(down: bool) {
    if !down {
        lock_tracker().c_up();
        return;
    }

    if let Some(deadline) = lock_tracker().c_down(Instant::now()) {
        schedule_tick(deadline);
    }
}

/// 键盘钩子：按住 Ctrl 时按了别的键。
#[cfg(target_os = "windows")]
pub(super) fn on_other_key() {
    lock_tracker().other_key();
}

/// watcher 的入库入口：替代直接调用 [`persist_and_notify`]。
pub fn dispatch(app: &AppHandle, item: ClipboardItem, source_app: Option<ClipboardApp>) {
    let merge = app
        .try_state::<SettingsStore>()
        .map(|store| store.snapshot().ry.merge_copy)
        .unwrap_or_default();
    let pending = Pending { item, source_app };

    if !cfg!(target_os = "windows") || !merge.enabled {
        persist_all(app.clone(), vec![pending]);
        return;
    }

    let is_text = pending.item.kind == ClipboardKind::Text;
    let (mut released, decision) = lock_tracker().on_change(Instant::now(), pending, is_text);

    match decision {
        Decision::Persist(pending) => released.push(pending),
        Decision::Hold { until } => schedule_tick(until),
        Decision::Merge(pending) => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                merge_into_latest(&app, pending, merge.separator, merge.sound).await;
            });
        }
        Decision::Drop => log::info!("merge copy: dropped duplicate clipboard write"),
    }

    persist_all(app.clone(), released);
}

fn schedule_tick(until: Instant) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(until.saturating_duration_since(Instant::now()) + TIMER_SLACK).await;
        let released = lock_tracker().tick(Instant::now());
        if let Some(app) = APP.get() {
            persist_all(app.clone(), released);
        }
    });
}

/// 按顺序入库；新图片入库后检查一次图片总占用上限。
fn persist_all(app: AppHandle, items: Vec<Pending>) {
    if items.is_empty() {
        return;
    }

    tauri::async_runtime::spawn(async move {
        let pool = app.state::<crate::db::DatabaseState>().pool().await;
        for pending in items {
            let is_image = pending.item.kind == ClipboardKind::Image;
            if let Err(err) =
                persist_and_notify(&app, &pool, &pending.item, pending.source_app.as_ref()).await
            {
                log::error!("clipboard watcher: persist failed: {err}");
                continue;
            }
            if is_image {
                super::image_quota::enforce(&app).await;
            }
        }
    });
}

/// 把新复制的文本接到最近一条历史后面，写回剪贴板并入库。任何一步不满足拼接条件，
/// 都退回为把新内容照常入库，保证复制的内容不会丢。
async fn merge_into_latest(
    app: &AppHandle,
    pending: Pending,
    separator: MergeSeparator,
    sound: bool,
) {
    let pool = app.state::<crate::db::DatabaseState>().pool().await;

    let previous = match find_latest_item(&pool).await {
        Ok(previous) => previous,
        Err(err) => {
            log::warn!("merge copy: find latest item failed: {err}");
            None
        }
    };
    let Some(previous) = previous else {
        persist_all(app.clone(), vec![pending]);
        return;
    };
    let (Some(previous_text), Some(addition)) = (plain_text(&previous), plain_text(&pending.item))
    else {
        log::info!("merge copy: latest item is not text, saving the new copy as is");
        persist_all(app.clone(), vec![pending]);
        return;
    };

    let merged_text = merge_text(previous_text, addition, separator);
    let Some(mut merged) = build_merged_item(app, merged_text) else {
        persist_all(app.clone(), vec![pending]);
        return;
    };
    merged.source_app_id = pending.item.source_app_id.clone();

    if let (Some(store), Some(guard)) = (
        app.try_state::<ImageStore>(),
        app.try_state::<std::sync::Arc<WritebackGuard>>(),
    ) {
        if let Err(err) = write_to_clipboard(&store, &guard, &merged, true) {
            log::warn!("merge copy: write merged text to clipboard failed: {err}");
        }
    }

    if !keeps_previous(&previous) {
        match delete_item(&pool, &previous.id).await {
            Ok(_) => {
                if let Err(err) = app.emit(CLIPBOARD_UPDATED_EVENT, json!({ "cleanup": 1 })) {
                    log::warn!("emit merge cleanup event failed: {err}");
                }
            }
            Err(err) => log::warn!("merge copy: delete previous item failed: {err}"),
        }
    }

    if let Err(err) = persist_and_notify(app, &pool, &merged, pending.source_app.as_ref()).await {
        log::error!("merge copy: persist merged item failed: {err}");
        return;
    }

    if sound {
        super::merge_sound::play(app);
    }
}

fn build_merged_item(app: &AppHandle, text: String) -> Option<ClipboardItem> {
    let settings = app
        .try_state::<SettingsStore>()
        .map(|store| store.snapshot())
        .unwrap_or_default();
    let store = app.try_state::<ImageStore>()?;
    let payload = ClipboardPayload::Text(TextPayload {
        text,
        html: None,
        rtf: None,
    });

    match build_item_with_settings(
        &store,
        &payload,
        &settings.clipboard.capture,
        &settings.clipboard.sensitive,
        true,
    ) {
        Ok(item) => item,
        Err(err) => {
            log::warn!("merge copy: build merged item failed: {err}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(base: Instant, millis: u64) -> Instant {
        base + Duration::from_millis(millis)
    }

    /// 按住 Ctrl，在 `at` 毫秒处按一下 C 再松开。
    fn press_c(tracker: &mut MergeTracker<&'static str>, at: Instant) {
        let _ = tracker.c_down(at);
        tracker.c_up();
    }

    #[test]
    fn double_press_merges_and_drops_the_intermediate_copy() {
        let t0 = Instant::now();
        let mut tracker = MergeTracker::default();
        tracker.ctrl_down();

        press_c(&mut tracker, t0);
        let (released, first) = tracker.on_change(ms(t0, 30), "first copy", true);
        assert!(released.is_empty());
        assert!(matches!(first, Decision::Hold { .. }));

        press_c(&mut tracker, ms(t0, 200));
        let (released, second) = tracker.on_change(ms(t0, 230), "second copy", true);

        assert!(released.is_empty(), "intermediate copy must not be saved");
        assert_eq!(second, Decision::Merge("second copy"));
        assert!(tracker.ctrl_up(ms(t0, 300)).is_empty());
    }

    #[test]
    fn single_copy_is_released_when_ctrl_is_let_go() {
        let t0 = Instant::now();
        let mut tracker = MergeTracker::default();
        tracker.ctrl_down();

        press_c(&mut tracker, t0);
        let (_, decision) = tracker.on_change(ms(t0, 30), "only copy", true);
        assert!(matches!(decision, Decision::Hold { .. }));

        assert_eq!(tracker.ctrl_up(ms(t0, 120)), ["only copy"]);
    }

    #[test]
    fn held_copy_is_released_after_the_press_window_while_ctrl_stays_down() {
        let t0 = Instant::now();
        let mut tracker = MergeTracker::default();
        tracker.ctrl_down();

        press_c(&mut tracker, t0);
        let (_, decision) = tracker.on_change(ms(t0, 30), "slow hand", true);
        let Decision::Hold { until } = decision else {
            panic!("expected hold, got {decision:?}");
        };
        assert_eq!(until, ms(t0, 500));

        assert!(tracker.tick(ms(t0, 400)).is_empty());
        assert_eq!(tracker.tick(ms(t0, 501)), ["slow hand"]);
    }

    #[test]
    fn held_copy_is_released_when_the_second_press_copies_nothing() {
        let t0 = Instant::now();
        let mut tracker = MergeTracker::default();
        tracker.ctrl_down();

        press_c(&mut tracker, t0);
        let (_, _) = tracker.on_change(ms(t0, 30), "first", true);
        let deadline = tracker.c_down(ms(t0, 200));
        tracker.c_up();
        assert_eq!(deadline, Some(ms(t0, 1_200)));

        // 还在等第二下带来的变化：Ctrl 松开也不放行。
        assert!(tracker.ctrl_up(ms(t0, 300)).is_empty());
        assert!(tracker.tick(ms(t0, 1_000)).is_empty());
        assert_eq!(tracker.tick(ms(t0, 1_201)), ["first"]);
    }

    #[test]
    fn presses_more_than_half_a_second_apart_do_not_merge() {
        let t0 = Instant::now();
        let mut tracker = MergeTracker::default();
        tracker.ctrl_down();

        press_c(&mut tracker, t0);
        let (_, _) = tracker.on_change(ms(t0, 30), "first", true);
        press_c(&mut tracker, ms(t0, 600));
        let (released, decision) = tracker.on_change(ms(t0, 630), "second", true);

        assert_eq!(released, ["first"]);
        assert!(matches!(decision, Decision::Hold { .. }));
    }

    #[test]
    fn terminal_double_ctrl_c_without_clipboard_change_does_nothing() {
        let t0 = Instant::now();
        let mut tracker = MergeTracker::default();
        tracker.ctrl_down();

        press_c(&mut tracker, t0);
        press_c(&mut tracker, ms(t0, 150));
        assert!(tracker.ctrl_up(ms(t0, 200)).is_empty());

        // 很久之后的一次普通复制不能被当成拼接。
        let (released, decision) = tracker.on_change(ms(t0, 5_000), "later copy", true);
        assert!(released.is_empty());
        assert_eq!(decision, Decision::Persist("later copy"));
    }

    #[test]
    fn clipboard_change_too_long_after_the_second_press_is_not_merged() {
        let t0 = Instant::now();
        let mut tracker = MergeTracker::default();
        tracker.ctrl_down();

        press_c(&mut tracker, t0);
        press_c(&mut tracker, ms(t0, 150));
        tracker.ctrl_up(ms(t0, 200));

        let (_, decision) = tracker.on_change(ms(t0, 1_300), "unrelated", true);
        assert_eq!(decision, Decision::Persist("unrelated"));
    }

    #[test]
    fn change_arriving_after_ctrl_release_still_merges_within_capture_window() {
        let t0 = Instant::now();
        let mut tracker = MergeTracker::default();
        tracker.ctrl_down();

        press_c(&mut tracker, t0);
        press_c(&mut tracker, ms(t0, 150));
        assert!(tracker.ctrl_up(ms(t0, 180)).is_empty());

        let (_, decision) = tracker.on_change(ms(t0, 400), "late copy", true);
        assert_eq!(decision, Decision::Merge("late copy"));
    }

    #[test]
    fn key_auto_repeat_is_not_a_second_press() {
        let t0 = Instant::now();
        let mut tracker = MergeTracker::default();
        tracker.ctrl_down();

        assert_eq!(tracker.c_down(t0), None);
        let (_, _) = tracker.on_change(ms(t0, 30), "held key", true);
        assert_eq!(tracker.c_down(ms(t0, 250)), None);
        assert_eq!(tracker.c_down(ms(t0, 280)), None);
        let (_, decision) = tracker.on_change(ms(t0, 300), "repeat", true);

        assert!(matches!(decision, Decision::Hold { .. }));
    }

    #[test]
    fn releasing_ctrl_between_presses_breaks_the_sequence() {
        let t0 = Instant::now();
        let mut tracker = MergeTracker::default();
        tracker.ctrl_down();
        press_c(&mut tracker, t0);
        let (_, _) = tracker.on_change(ms(t0, 20), "first", true);
        assert_eq!(tracker.ctrl_up(ms(t0, 80)), ["first"]);

        tracker.ctrl_down();
        press_c(&mut tracker, ms(t0, 200));
        let (_, decision) = tracker.on_change(ms(t0, 230), "second", true);

        assert!(matches!(decision, Decision::Hold { .. }));
    }

    #[test]
    fn another_key_between_presses_breaks_the_sequence() {
        let t0 = Instant::now();
        let mut tracker = MergeTracker::default();
        tracker.ctrl_down();
        press_c(&mut tracker, t0);
        tracker.other_key();
        press_c(&mut tracker, ms(t0, 150));
        let (_, decision) = tracker.on_change(ms(t0, 180), "copy", true);

        assert!(matches!(decision, Decision::Hold { .. }));
    }

    #[test]
    fn second_write_from_the_merged_press_is_dropped() {
        let t0 = Instant::now();
        let mut tracker = MergeTracker::default();
        tracker.ctrl_down();
        press_c(&mut tracker, t0);
        let (_, _) = tracker.on_change(ms(t0, 20), "first", true);
        press_c(&mut tracker, ms(t0, 150));
        let (_, merged) = tracker.on_change(ms(t0, 170), "second", true);
        assert_eq!(merged, Decision::Merge("second"));

        let (released, duplicate) = tracker.on_change(ms(t0, 190), "second", true);
        assert!(released.is_empty());
        assert_eq!(duplicate, Decision::Drop);

        // 拼接窗口过后的复制恢复正常。
        tracker.ctrl_up(ms(t0, 250));
        let (_, later) = tracker.on_change(ms(t0, 2_000), "later", true);
        assert_eq!(later, Decision::Persist("later"));
    }

    #[test]
    fn non_text_copies_are_never_held_or_merged() {
        let t0 = Instant::now();
        let mut tracker = MergeTracker::default();
        tracker.ctrl_down();

        press_c(&mut tracker, t0);
        let (_, first) = tracker.on_change(ms(t0, 20), "image", false);
        assert_eq!(first, Decision::Persist("image"));

        press_c(&mut tracker, ms(t0, 150));
        let (_, second) = tracker.on_change(ms(t0, 170), "image again", false);
        assert_eq!(second, Decision::Persist("image again"));
    }

    #[test]
    fn copy_without_ctrl_held_is_persisted_immediately() {
        let t0 = Instant::now();
        let mut tracker: MergeTracker<&str> = MergeTracker::default();

        let (_, decision) = tracker.on_change(t0, "menu copy", true);

        assert_eq!(decision, Decision::Persist("menu copy"));
    }

    #[test]
    fn merge_text_uses_configured_separator() {
        assert_eq!(merge_text("foo", "bar", MergeSeparator::Space), "foo bar");
        assert_eq!(
            merge_text("foo", "bar", MergeSeparator::Newline),
            "foo\nbar"
        );
    }

    fn text_item(content: &str) -> ClipboardItem {
        let ts = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        ClipboardItem {
            id: "id".to_owned(),
            kind: ClipboardKind::Text,
            sub_kind: None,
            group_id: None,
            source_app_id: None,
            content: content.to_owned(),
            content_hash: String::new(),
            search_text: None,
            summary: None,
            file_types: None,
            size: None,
            width: None,
            height: None,
            use_count: 1,
            is_favorite: false,
            is_pinned: false,
            is_sensitive: false,
            platform: crate::db::models::Platform::Windows,
            note: None,
            created_at: ts,
            updated_at: ts,
            source_app_name: None,
            source_app_icon_file: None,
            source_app_icon_path: None,
            image_thumbnail_path: None,
            file_entries: None,
            files_preview_kind: None,
            available_actions: Vec::new(),
            color_preview: None,
            display_created_at: String::new(),
        }
    }

    #[test]
    fn plain_text_reads_search_text_for_rich_text_items() {
        let mut html = text_item("<b>bold</b>");
        html.sub_kind = Some(ClipboardSubKind::Html);
        html.search_text = Some("bold".to_owned());
        let mut url = text_item("https://example.com");
        url.sub_kind = Some(ClipboardSubKind::Url);
        let mut image = text_item("cafe.png");
        image.kind = ClipboardKind::Image;

        assert_eq!(plain_text(&html), Some("bold"));
        assert_eq!(plain_text(&url), Some("https://example.com"));
        assert_eq!(plain_text(&image), None);
    }

    #[test]
    fn previous_item_is_kept_only_when_user_curated_it() {
        let plain = text_item("plain");
        let mut pinned = text_item("pinned");
        pinned.is_pinned = true;
        let mut noted = text_item("noted");
        noted.note = Some("keep".to_owned());

        assert!(!keeps_previous(&plain));
        assert!(keeps_previous(&pinned));
        assert!(keeps_previous(&noted));
    }
}
