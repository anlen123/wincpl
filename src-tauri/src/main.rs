#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
mod app;
mod config;
#[cfg_attr(not(windows), allow(dead_code))]
mod layout;
mod ocr;
#[cfg(windows)]
mod platform;
mod relay;
mod store;

#[cfg(windows)]
fn main() {
    app::run();
}

#[cfg(not(windows))]
fn main() {
    eprintln!("剪藏仅支持 Windows 11。当前平台可运行存储与手机接入测试，以及浏览器界面预览。");
    std::process::exit(1);
}
