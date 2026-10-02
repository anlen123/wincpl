//! 只读检查实际用于手机配对的地址：cargo run --example lan-address。
#[cfg(windows)]
#[path = "../src/lan.rs"]
mod lan;

#[cfg(windows)]
fn main() {
    match lan::lan_ipv4() {
        Some(ip) => println!("手机配对局域网地址：{ip}"),
        None => {
            eprintln!("未找到已连接的物理 Wi-Fi/以太网私有 IPv4 地址");
            std::process::exit(1);
        }
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("请在 Windows 本机运行此网卡诊断工具");
    std::process::exit(1);
}
