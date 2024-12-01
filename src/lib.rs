pub mod core;

/// Re-export backend `ffmpeg` library.
pub use ffmpeg_next as ffmpeg;

// 导出ffmpeg_next crate，用于后续的多媒体处理功能
pub extern crate ffmpeg_next;

// 导出ndarray crate，提供多维数组支持，用于数据处理
pub extern crate ndarray;

// 导出url crate，用于处理和解析URL
pub extern crate url;
