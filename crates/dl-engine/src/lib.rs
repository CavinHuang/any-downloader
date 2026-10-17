//! 下载引擎：任务生命周期、下载驱动编排、事件流。无 UI 依赖，可在 CLI 下独立运行与压测。

pub mod checksum;
pub mod driver;
pub mod engine;
pub mod hls;
pub mod postprocess;
pub mod ytdlp_task;

pub use driver::{DownloadOptions, MAX_SEGMENTS};
pub use engine::{Engine, EngineEvent};
pub use hls::find_ffmpeg;

/// 应用数据目录（可用 ANYTHING_DL_DATA_DIR 覆盖）：
/// Windows %APPDATA%\anything-downloader，macOS ~/Library/Application Support/anything-downloader。
pub fn data_dir() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("ANYTHING_DL_DATA_DIR") {
        return std::path::PathBuf::from(dir);
    }
    dirs::data_dir()
        .map(|d| d.join("anything-downloader"))
        .unwrap_or_else(|| std::env::temp_dir().join("anything-downloader"))
}

/// 自动更新二进制的托管目录（数据目录下：可写、跨应用版本存活）。
pub fn managed_bin_dir() -> std::path::PathBuf {
    data_dir().join("managed")
}

/// 托管 yt-dlp 的文件名（平台后缀）。
pub fn managed_ytdlp_name() -> &'static str {
    if cfg!(windows) {
        "yt-dlp.exe"
    } else {
        "yt-dlp"
    }
}
