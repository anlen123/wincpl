//! 手机短信验证码接入：验证码解析、配对载荷与局域网 HTTP 服务。
//!
//! 本模块与平台无关（不依赖 Windows API），因此可以在任意平台运行单元测试：
//! 验证码提取、鉴权、路由与真实的 socket 往返都在这里覆盖。
//! 手机 App 的流程是：扫二维码拿到 `jianzang://pair?...`，
//! 之后每次收到短信就 POST `/api/v1/sms`，请求头带 `X-Jianzang-Token`。

use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hasher},
    io::{BufRead, BufReader, Read, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// 请求体上限：验证码短信很短，超过这个体积直接拒绝，避免被当作内存放大器。
pub const MAX_BODY_BYTES: usize = 8 * 1024;
/// 请求头总长度上限。
const MAX_HEADER_BYTES: usize = 8 * 1024;
/// 短信正文保留的字符数上限（只用于提示与兜底解析，不落库）。
pub const MAX_TEXT_CHARS: usize = 600;
const READ_TIMEOUT: Duration = Duration::from_secs(6);
/// 监听线程的轮询间隔，用于在不引入异步运行时的前提下响应“停用”开关。
const ACCEPT_POLL: Duration = Duration::from_millis(40);

const KEYWORDS: [&str; 8] = [
    "验证码",
    "校验码",
    "动态码",
    "确认码",
    "验证代码",
    "口令",
    "code",
    "pin",
];

/// 关键词和数字之间常见的连接字，例如“验证码是1234”“验证码：1234”。
const FILLER_LIMIT: usize = 12;
/// 一句结束还没出现数字就放弃，例如“验证码已发送，请查收”。
const SENTENCE_ENDS: [char; 6] = ['。', '，', '！', '？', '；', '\n'];

/// 一次成功接收到的验证码。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelaySms {
    pub code: String,
    pub text: String,
    pub from: Option<String>,
    pub received_at: u64,
}

/// 一次请求的处理结果：HTTP 状态码、响应体，以及需要注入剪贴板的验证码。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayReply {
    pub status: u16,
    pub json: String,
    pub sms: Option<RelaySms>,
}

impl RelayReply {
    fn ok(json: String, sms: Option<RelaySms>) -> Self {
        Self {
            status: 200,
            json,
            sms,
        }
    }

    fn reject(status: u16, error: &str) -> Self {
        Self {
            status,
            json: serde_json::json!({ "ok": false, "error": error }).to_string(),
            sms: None,
        }
    }
}

/// 服务端上下文：鉴权令牌与服务信息。
#[derive(Clone, Debug)]
pub struct RelayContext {
    pub token: String,
    pub version: String,
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Deserialize)]
struct SmsBody {
    #[serde(default)]
    code: Option<String>,
    #[serde(default, alias = "message", alias = "body")]
    text: Option<String>,
    #[serde(default, alias = "sender", alias = "address")]
    from: Option<String>,
}

/// 生成 128 位配对令牌（32 位十六进制）。
///
/// `RandomState` 的种子来自操作系统的随机数池，再叠加进程号与时间戳后做 SHA-256，
/// 因此不需要引入额外的随机数依赖。
pub fn random_token() -> String {
    let mut hasher = Sha256::new();
    for _ in 0..2 {
        let mut state = RandomState::new().build_hasher();
        state.write_u64(0x9e37_79b9_7f4a_7c15);
        state.write_u64(SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64));
        hasher.update(state.finish().to_le_bytes());
    }
    hasher.update(std::process::id().to_le_bytes());
    hasher.update(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos())
            .to_le_bytes(),
    );
    let digest = hasher.finalize();
    let mut token = String::with_capacity(32);
    for byte in digest.iter().take(16) {
        token.push_str(&format!("{byte:02x}"));
    }
    token
}

pub fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

/// 找出本机用来出网的局域网 IPv4 地址；没有默认路由时返回 None。
pub fn lan_ipv4() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    // UDP connect 不会发包，只是让系统按路由选出网卡。
    socket.connect((Ipv4Addr::new(8, 8, 8, 8), 80)).ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if !ip.is_loopback() => Some(ip),
        _ => None,
    }
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        let unreserved = byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~');
        if unreserved {
            encoded.push(*byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// 二维码里的配对载荷：手机扫码后据此保存地址与令牌。
pub fn pairing_payload(host: &str, port: u16, token: &str, name: &str) -> String {
    format!(
        "jianzang://pair?host={host}&port={port}&token={token}&name={}",
        percent_encode(name)
    )
}

/// 把配对载荷渲染成 SVG 二维码，返回可以直接放进 `<img src>` 的 data URI。
pub fn qr_data_uri(payload: &str) -> Result<String, String> {
    let code = qrcode::QrCode::new(payload.as_bytes())
        .map_err(|error| format!("无法生成二维码：{error}"))?;
    let mut svg = code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(240, 240)
        .quiet_zone(true)
        .build();
    if !svg.contains("xmlns") {
        svg = svg.replacen("<svg", "<svg xmlns=\"http://www.w3.org/2000/svg\"", 1);
    }
    Ok(format!(
        "data:image/svg+xml;charset=utf-8,{}",
        percent_encode(&svg)
    ))
}

/// 从短信正文里提取验证码：先看关键词后面的数字，再退化为“唯一一段 4~8 位数字”。
pub fn extract_code(text: &str) -> Option<String> {
    let ascii = text.to_ascii_lowercase();
    for keyword in KEYWORDS {
        let mut from = 0;
        while let Some(position) = ascii[from..].find(keyword) {
            let start = from + position + keyword.len();
            if let Some(code) = digits_after(text, start) {
                return Some(code);
            }
            from = start.max(from + 1);
            if from >= ascii.len() {
                break;
            }
        }
    }
    let mut fallback: Option<&str> = None;
    let mut short: Option<&str> = None;
    for (start, end) in digit_runs(text) {
        let run = &text[start..end];
        if !(4..=8).contains(&run.len()) || looks_like_year(run) {
            continue;
        }
        if fallback.is_none() {
            fallback = Some(run);
        }
        if run.len() <= 6 {
            short = Some(run);
            break;
        }
    }
    short.or(fallback).map(str::to_string)
}

/// 关键词之后的数字：允许跳过“是/为/：”等连接字，也允许 `123 456` 这样被空格分开。
fn digits_after(text: &str, start: usize) -> Option<String> {
    let mut digits = String::new();
    let mut filler = 0;
    for character in text[start.min(text.len())..].chars().take(64) {
        if character.is_ascii_digit() {
            if digits.len() == 8 {
                // 超过 8 位说明这是手机号、订单号一类的长数字，不是验证码。
                return None;
            }
            digits.push(character);
            continue;
        }
        if digits.is_empty() {
            if SENTENCE_ENDS.contains(&character) {
                return None;
            }
            filler += 1;
            if filler > FILLER_LIMIT {
                return None;
            }
            continue;
        }
        // 已经有数字：空格或短横线可能是“123 456”这种分隔，其余字符表示数字已结束。
        if character.is_whitespace() || matches!(character, '-' | '\u{2013}' | '\u{FF0D}') {
            continue;
        }
        break;
    }
    (4..=8).contains(&digits.len()).then_some(digits)
}

/// 文本里所有连续数字段的字节区间。
fn digit_runs(text: &str) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let mut start: Option<usize> = None;
    for (index, character) in text.char_indices() {
        if character.is_ascii_digit() {
            start.get_or_insert(index);
        } else if let Some(begin) = start.take() {
            runs.push((begin, index));
        }
    }
    if let Some(begin) = start {
        runs.push((begin, text.len()));
    }
    runs
}

fn looks_like_year(value: &str) -> bool {
    value.len() == 4 && (value.starts_with("19") || value.starts_with("20"))
}

/// 比较令牌，长度不同直接失败，避免用 `==` 泄露前缀匹配进度。
fn token_matches(candidate: &str, expected: &str) -> bool {
    let (left, right) = (candidate.as_bytes(), expected.as_bytes());
    if left.len() != right.len() || right.is_empty() {
        return false;
    }
    let mut difference = 0_u8;
    for (a, b) in left.iter().zip(right) {
        difference |= a ^ b;
    }
    difference == 0
}

fn query_token(target: &str) -> Option<String> {
    let query = target.split_once('?')?.1;
    for pair in query.split('&') {
        if let Some((key, value)) = pair.split_once('=') {
            if key.eq_ignore_ascii_case("token") && !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// 处理一条已经解析好的请求。纯函数，方便测试与复用。
pub fn route(
    context: &RelayContext,
    method: &str,
    target: &str,
    header_token: Option<&str>,
    body: &str,
) -> RelayReply {
    if context.token.is_empty() {
        return RelayReply::reject(503, "手机接入尚未启用");
    }
    let provided = header_token
        .map(str::to_string)
        .or_else(|| query_token(target));
    let authorized = provided
        .as_deref()
        .is_some_and(|token| token_matches(token, &context.token));
    if !authorized {
        return RelayReply::reject(401, "配对令牌无效，请在电脑端重新扫码");
    }

    let path = target.split('?').next().unwrap_or("").trim_end_matches('/');
    match (method, path) {
        ("GET", "/api/v1/ping") => RelayReply::ok(
            serde_json::json!({
                "ok": true,
                "app": "jianzang",
                "version": context.version,
                "host": context.host,
                "port": context.port,
            })
            .to_string(),
            None,
        ),
        ("POST", "/api/v1/sms") => {
            if body.len() > MAX_BODY_BYTES {
                return RelayReply::reject(413, "请求体过大");
            }
            let parsed: SmsBody = match serde_json::from_str(body) {
                Ok(parsed) => parsed,
                Err(error) => {
                    return RelayReply::reject(400, &format!("请求体不是合法的 JSON：{error}"));
                }
            };
            let text: String = parsed
                .text
                .unwrap_or_default()
                .chars()
                .take(MAX_TEXT_CHARS)
                .collect();
            let code = parsed
                .code
                .as_deref()
                .map(digits_only)
                .filter(|code| (4..=8).contains(&code.len()))
                .or_else(|| extract_code(&text));
            let Some(code) = code else {
                return RelayReply::reject(422, "没有从短信里识别到验证码");
            };
            let from = parsed
                .from
                .filter(|value| !value.trim().is_empty())
                .map(|value| value.chars().take(32).collect::<String>());
            let sms = RelaySms {
                code: code.clone(),
                text,
                from,
                received_at: now_seconds(),
            };
            RelayReply::ok(
                serde_json::json!({ "ok": true, "code": code }).to_string(),
                Some(sms),
            )
        }
        _ => RelayReply::reject(404, "没有这个接口"),
    }
}

fn digits_only(value: &str) -> String {
    value.chars().filter(char::is_ascii_digit).collect()
}

/// 在后台线程里跑监听循环，直到 `stop` 被置位。
pub fn serve(
    listener: TcpListener,
    context: Arc<RelayContext>,
    stop: Arc<AtomicBool>,
    on_sms: impl Fn(RelaySms),
) -> std::io::Result<()> {
    listener.set_nonblocking(true)?;
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => {
                let _ = handle_connection(stream, &context, &on_sms);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(ACCEPT_POLL);
            }
            Err(_) => std::thread::sleep(Duration::from_millis(200)),
        }
    }
    Ok(())
}

/// 绑定端口。0.0.0.0 使同一局域网内的手机都能访问。
pub fn bind(port: u16) -> Result<TcpListener, String> {
    TcpListener::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)))
        .map_err(|error| format!("无法监听端口 {port}：{error}"))
}

fn handle_connection(
    stream: TcpStream,
    context: &RelayContext,
    on_sms: &impl Fn(RelaySms),
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    stream.set_write_timeout(Some(READ_TIMEOUT))?;
    let mut reader = BufReader::new(stream.try_clone()?);

    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();

    let mut content_length = 0_usize;
    let mut header_bytes = request_line.len();
    let mut header_token: Option<String> = None;
    let mut oversized = false;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        header_bytes += line.len();
        if header_bytes > MAX_HEADER_BYTES {
            return write_response(&stream, 431, "{\"ok\":false,\"error\":\"请求头过大\"}");
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            let name = name.trim();
            let value = value.trim();
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.parse().unwrap_or(0);
                oversized = content_length > MAX_BODY_BYTES;
            } else if name.eq_ignore_ascii_case("x-jianzang-token") {
                header_token = Some(value.to_string());
            }
        }
    }

    let reply = if oversized {
        RelayReply::reject(413, "请求体过大")
    } else {
        let mut raw = vec![0_u8; content_length];
        if content_length > 0 {
            reader.read_exact(&mut raw)?;
        }
        let body = String::from_utf8_lossy(&raw);
        route(context, &method, &target, header_token.as_deref(), &body)
    };
    if let Some(sms) = reply.sms.clone() {
        on_sms(sms);
    }
    write_response(&stream, reply.status, &reply.json)
}

fn write_response(mut stream: &TcpStream, status: u16, json: &str) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        413 => "Content Too Large",
        422 => "Unprocessable Content",
        431 => "Request Header Fields Too Large",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json; charset=utf-8\r\n\
         Content-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{json}",
        json.len()
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> RelayContext {
        RelayContext {
            token: "0123456789abcdef0123456789abcdef".into(),
            version: "0.1.0".into(),
            host: "DESKTOP-AB".into(),
            port: 8788,
        }
    }

    #[test]
    fn extracts_code_next_to_keyword() {
        assert_eq!(
            extract_code("【示例】您的验证码是 123456，5 分钟内有效。").as_deref(),
            Some("123456")
        );
        assert_eq!(
            extract_code("验证码：8888。请勿泄露").as_deref(),
            Some("8888")
        );
        assert_eq!(
            extract_code("您的校验码为 4321，如非本人操作请忽略").as_deref(),
            Some("4321")
        );
        assert_eq!(
            extract_code("Your verification CODE is 5678.").as_deref(),
            Some("5678")
        );
    }

    #[test]
    fn extracts_spaced_and_grouped_codes() {
        assert_eq!(
            extract_code("验证码 1 2 3 4 5 6，请在两分钟内输入").as_deref(),
            Some("123456")
        );
        assert_eq!(
            extract_code("验证码：123 456").as_deref(),
            Some("123456")
        );
        assert_eq!(extract_code("验证码为 12-34").as_deref(), Some("1234"));
    }

    #[test]
    fn falls_back_to_lone_digit_run_without_keyword() {
        assert_eq!(extract_code("4321").as_deref(), Some("4321"));
        assert_eq!(extract_code("订单 8891 已完成").as_deref(), Some("8891"));
        assert_eq!(extract_code("您有 1 条新消息").as_deref(), None);
    }

    #[test]
    fn ignores_phone_numbers_years_and_long_runs() {
        assert_eq!(extract_code("请拨打电话 13800138000 咨询"), None);
        assert_eq!(
            extract_code("快递单号 1234567890123 已揽收").as_deref(),
            None
        );
        assert_eq!(extract_code("2024 年 5 月").as_deref(), None);
        assert_eq!(
            extract_code("订单 2024 的验证码是 889900").as_deref(),
            Some("889900")
        );
    }

    #[test]
    fn rejects_promotional_sentences() {
        assert_eq!(extract_code("验证码已发送，请查收"), None);
        assert_eq!(extract_code(""), None);
    }

    #[test]
    fn pairing_payload_and_qr_are_usable() {
        let payload = pairing_payload("192.168.1.7", 8788, "abcdef0123456789", "DESKTOP AB");
        assert!(payload.starts_with("jianzang://pair?host=192.168.1.7&port=8788&token="));
        assert!(payload.contains("name=DESKTOP%20AB"));
        let uri = qr_data_uri(&payload).unwrap();
        assert!(uri.starts_with("data:image/svg+xml;charset=utf-8,"));
        assert!(uri.contains("xmlns%3D%22http"));
    }

    #[test]
    fn token_is_random_and_long_enough() {
        let first = random_token();
        let second = random_token();
        assert_eq!(first.len(), 32);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(first, second);
    }

    #[test]
    fn route_requires_the_pairing_token() {
        let context = context();
        let unauthorized = route(&context, "GET", "/api/v1/ping", None, "");
        assert_eq!(unauthorized.status, 401);
        let wrong = route(&context, "GET", "/api/v1/ping", Some("deadbeef"), "");
        assert_eq!(wrong.status, 401);
        let disabled = route(
            &RelayContext {
                token: String::new(),
                ..context.clone()
            },
            "GET",
            "/api/v1/ping",
            Some("anything"),
            "",
        );
        assert_eq!(disabled.status, 503);
    }

    #[test]
    fn route_accepts_ping_from_header_or_query() {
        let context = context();
        let by_header = route(
            &context,
            "GET",
            "/api/v1/ping",
            Some(&context.token.clone()),
            "",
        );
        assert_eq!(by_header.status, 200);
        assert!(by_header.json.contains("\"app\":\"jianzang\""));

        let target = format!("/api/v1/ping?token={}", context.token);
        let by_query = route(&context, "GET", &target, None, "");
        assert_eq!(by_query.status, 200);
    }

    #[test]
    fn route_extracts_code_from_sms_body() {
        let context = context();
        let body = r#"{"text":"【示例】您的验证码是 654321，请勿泄露","from":"10690000"}"#;
        let reply = route(
            &context,
            "POST",
            "/api/v1/sms",
            Some(&context.token.clone()),
            body,
        );
        assert_eq!(reply.status, 200);
        assert!(reply.json.contains("\"code\":\"654321\""));
        let sms = reply.sms.expect("should accept");
        assert_eq!(sms.code, "654321");
        assert_eq!(sms.from.as_deref(), Some("10690000"));
        assert!(sms.received_at > 1_600_000_000);
    }

    #[test]
    fn route_reports_bad_payloads() {
        let context = context();
        let token = context.token.clone();
        let bad_json = route(&context, "POST", "/api/v1/sms", Some(&token), "{oops");
        assert_eq!(bad_json.status, 400);

        let no_code = route(&context, "POST", "/api/v1/sms", Some(&token), "{\"text\":\"你好\"}");
        assert_eq!(no_code.status, 422);

        let unknown = route(&context, "GET", "/api/v1/other", Some(&token), "");
        assert_eq!(unknown.status, 404);

        let huge = "x".repeat(MAX_BODY_BYTES + 1);
        let too_big = route(&context, "POST", "/api/v1/sms", Some(&token), &huge);
        assert_eq!(too_big.status, 413);
    }

    #[test]
    fn serves_a_real_round_trip_over_tcp() {
        let listener = bind(0).unwrap();
        let address = listener.local_addr().unwrap();
        let context = Arc::new(context());
        let stop = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = seen.clone();
        let server_stop = stop.clone();
        let server_context = context.clone();
        let handle = std::thread::spawn(move || {
            serve(listener, server_context, server_stop, move |sms| {
                recorder.lock().unwrap().push(sms);
            })
        });

        let body = r#"{"text":"验证码 998877"}"#;
        let request = format!(
            "POST /api/v1/sms HTTP/1.1\r\nHost: {address}\r\nX-Jianzang-Token: {}\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            context.token,
            body.len()
        );
        let mut stream = TcpStream::connect(address).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        stream.write_all(body.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
        assert!(response.contains("\"code\":\"998877\""), "{response}");

        // 没有令牌的请求同样走完整链路，应当被拒绝。
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .write_all(b"GET /api/v1/ping HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        let mut denied = String::new();
        stream.read_to_string(&mut denied).unwrap();
        assert!(denied.starts_with("HTTP/1.1 401 Unauthorized"), "{denied}");

        stop.store(true, Ordering::Release);
        handle.join().unwrap().unwrap();
        let recorded = seen.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].code, "998877");
    }
}
