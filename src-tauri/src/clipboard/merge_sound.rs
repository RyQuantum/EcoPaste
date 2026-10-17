//! 拼接复制成功提示音（fork 补丁）。
//!
//! 优先播放用户自己放进数据目录的音效：`<数据目录>/config/sounds/merge.{aiff,aif,wav,mp3}`
//! （例如从 Mac 拷来的 `/System/Library/Sounds/Purr.aiff`，Alfred 拼接时播放的就是它）。
//! 没有或解码失败时，用正弦波现场合成两个上扬短音。不在仓库里放音频文件：
//! Alfred 用的是 macOS 系统音效，不能放进公开仓库。
//! 播放方式与 `sound.rs` 一致：每次新开短命线程建流播放，失败只记日志。
//! 局域网同步的「收到内容」提示音（`receive.*`）也走这里。

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rodio::source::{SineWave, Source};
use rodio::{Decoder, OutputStream, Sink};
use tauri::AppHandle;

/// 自备音效的文件名（不含扩展名）与按顺序尝试的扩展名。
const CUSTOM_SOUND_STEM: &str = "merge";
const CUSTOM_SOUND_EXTENSIONS: [&str; 4] = ["aiff", "aif", "wav", "mp3"];
const SOUNDS_DIR: &str = "sounds";

/// (频率 Hz, 时长 ms)：E6 → A6，四度上行。
const NOTES: [(f32, u64); 2] = [(1318.5, 70), (1760.0, 110)];
/// 收到远程内容：A6 → E6，下行，和拼接音区分开。
const RECEIVE_NOTES: [(f32, u64); 2] = [(1760.0, 70), (1318.5, 120)];
const RECEIVE_SOUND_STEM: &str = "receive";
const VOLUME: f32 = 0.18;

/// 异步播放一次拼接提示音。
pub fn play(app: &AppHandle) {
    play_custom_or_tone(app, CUSTOM_SOUND_STEM, &NOTES);
}

/// 异步播放一次「收到其他设备的内容」提示音：`config/sounds/receive.*`，
/// 例如 CrossPaste 收到内容时播放的 Blow.wav。
pub fn play_receive(app: &AppHandle) {
    play_custom_or_tone(app, RECEIVE_SOUND_STEM, &RECEIVE_NOTES);
}

fn play_custom_or_tone(app: &AppHandle, stem: &'static str, notes: &'static [(f32, u64)]) {
    let custom = crate::core::paths::config_dir(app)
        .ok()
        .and_then(|dir| find_custom_sound(&dir.join(SOUNDS_DIR), stem));

    std::thread::Builder::new()
        .name(format!("{stem}-sound"))
        .spawn(move || {
            if let Err(err) = play_blocking(custom.as_deref(), notes) {
                log::warn!("play {stem} sound failed: {err}");
            }
        })
        .ok();
}

/// 在 `dir` 里按扩展名顺序找第一个存在的 `<stem>.*`。
fn find_custom_sound(dir: &Path, stem: &str) -> Option<PathBuf> {
    CUSTOM_SOUND_EXTENSIONS
        .iter()
        .map(|ext| dir.join(format!("{stem}.{ext}")))
        .find(|path| path.is_file())
}

fn play_blocking(custom: Option<&Path>, notes: &[(f32, u64)]) -> Result<(), String> {
    let (_stream, handle) = OutputStream::try_default().map_err(|e| e.to_string())?;
    let sink = Sink::try_new(&handle).map_err(|e| e.to_string())?;

    match custom.map(decode_file) {
        Some(Ok(source)) => sink.append(source),
        Some(Err(err)) => {
            log::warn!("custom sound unusable, falling back to built-in tone: {err}");
            append_tone(&sink, notes);
        }
        None => append_tone(&sink, notes),
    }

    sink.sleep_until_end();
    Ok(())
}

fn decode_file(path: &Path) -> Result<Decoder<Cursor<Vec<u8>>>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{path:?}: {e}"))?;
    Decoder::new(Cursor::new(bytes)).map_err(|e| format!("{path:?}: {e}"))
}

fn append_tone(sink: &Sink, notes: &[(f32, u64)]) {
    for &(frequency, millis) in notes {
        let mut note = SineWave::new(frequency)
            .amplify(VOLUME)
            .fade_in(Duration::from_millis(5))
            .take_duration(Duration::from_millis(millis));
        // 结尾淡出，避免波形被硬切断时的爆音。
        note.set_filter_fadeout();
        sink.append(note);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_sound_prefers_aiff_and_ignores_other_names() {
        let dir =
            std::env::temp_dir().join(format!("ecopaste-merge-sound-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        assert_eq!(find_custom_sound(&dir, "merge"), None);

        std::fs::write(dir.join("Purr.aiff"), b"x").unwrap();
        std::fs::write(dir.join("merge.wav"), b"x").unwrap();
        assert_eq!(
            find_custom_sound(&dir, "merge"),
            Some(dir.join("merge.wav"))
        );

        std::fs::write(dir.join("merge.aiff"), b"x").unwrap();
        assert_eq!(
            find_custom_sound(&dir, "merge"),
            Some(dir.join("merge.aiff"))
        );
        assert_eq!(find_custom_sound(&dir, "receive"), None);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[ignore = "needs a real sound file; set ECOPASTE_TEST_SOUND to e.g. a copy of Purr.aiff"]
    fn decodes_a_real_custom_sound_file() {
        let path = std::env::var("ECOPASTE_TEST_SOUND").expect("ECOPASTE_TEST_SOUND");
        let source = decode_file(Path::new(&path)).unwrap();

        assert!(source.sample_rate() > 0);
        assert!(source.count() > 1000, "decoded too few samples");
    }
}
