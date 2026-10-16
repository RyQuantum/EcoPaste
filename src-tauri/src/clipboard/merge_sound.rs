//! 拼接复制成功提示音（fork 补丁）：两个上扬的短音，和普通复制提示音区分开。
//!
//! 用正弦波现场合成，不带音频文件：Alfred 的原版音效是它自己的素材，不能放进公开仓库。
//! 播放方式与 `sound.rs` 一致：每次新开短命线程建流播放，失败只记日志。

use std::time::Duration;

use rodio::source::{SineWave, Source};
use rodio::{OutputStream, Sink};

/// (频率 Hz, 时长 ms)：E6 → A6，四度上行。
const NOTES: [(f32, u64); 2] = [(1318.5, 70), (1760.0, 110)];
const VOLUME: f32 = 0.18;

/// 异步播放一次拼接提示音。
pub fn play() {
    std::thread::Builder::new()
        .name("merge-sound".into())
        .spawn(|| {
            if let Err(err) = play_blocking() {
                log::warn!("play merge sound failed: {err}");
            }
        })
        .ok();
}

fn play_blocking() -> Result<(), String> {
    let (_stream, handle) = OutputStream::try_default().map_err(|e| e.to_string())?;
    let sink = Sink::try_new(&handle).map_err(|e| e.to_string())?;

    for (frequency, millis) in NOTES {
        let mut note = SineWave::new(frequency)
            .amplify(VOLUME)
            .fade_in(Duration::from_millis(5))
            .take_duration(Duration::from_millis(millis));
        // 结尾淡出，避免波形被硬切断时的爆音。
        note.set_filter_fadeout();
        sink.append(note);
    }

    sink.sleep_until_end();
    Ok(())
}
