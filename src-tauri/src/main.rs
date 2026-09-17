#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
mod app;
mod config;
mod ocr;
#[cfg(windows)]
mod platform;
mod store;

#[cfg(windows)]
fn main() {
    app::run();
}

#[cfg(not(windows))]
fn main() {
    eprintln!("剪藏仅支持 Windows 11。当前平台可运行存储测试和浏览器界面预览。");
    std::process::exit(1);
}
