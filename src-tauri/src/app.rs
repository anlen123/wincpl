use crate::{
    config::Config,
    platform,
    relay::{self, RelayContext, RelaySms},
    store::{Entry, Snippet, Store, TagCount},
};
use std::{
    borrow::Cow,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering},
        mpsc::{sync_channel, SyncSender},
        Arc, Mutex, MutexGuard,
    },
    time::Duration,
};
use tauri::{Emitter, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

struct AppState {
    ready: AtomicBool,
    config: Mutex<Config>,
    store: Mutex<Store>,
    config_path: PathBuf,
    target: AtomicIsize,
    own_sequence: AtomicU32,
    clipboard_gate: Mutex<()>,
    pasting: AtomicBool,
    ocr_wake: SyncSender<()>,
    last_error: Mutex<Option<String>>,
    snippet_mode: AtomicBool,
    preview: Mutex<Option<Entry>>,
    preview_terms: Mutex<Vec<String>>,
    toast_sequence: AtomicU32,
    toast_generation: AtomicU32,
    phone_server: Mutex<Option<relay::RelayServer>>,
    phone_state: Mutex<PhoneRuntime>,
}

/// 右下角“已复制”提示的内容：一段预览文字加上长度/尺寸说明。
#[derive(Clone, serde::Serialize)]
struct ToastPayload {
    kind: String,
    preview: String,
    detail: String,
    duration_ms: u32,
}

/// 预览窗口返回的数据：条目本身，加上当前搜索关键词，用于在浮窗内高亮。
#[derive(Clone, serde::Serialize)]
struct PreviewPayload {
    entry: Option<Entry>,
    terms: Vec<String>,
}

/// 把搜索框内容拆成用于高亮的关键词：按空白分隔、转小写、去重并限制数量。
fn preview_terms(query: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    for term in query.split_whitespace() {
        if term.is_empty() || term.chars().count() > 64 {
            continue;
        }
        let lowered = term.to_lowercase();
        if !terms.contains(&lowered) {
            terms.push(lowered);
        }
        if terms.len() >= 8 {
            break;
        }
    }
    terms
}

fn lock<T>(value: &Mutex<T>) -> Result<MutexGuard<'_, T>, String> {
    value
        .lock()
        .map_err(|_| "内部状态锁已损坏，请重新启动剪藏".into())
}

/// 把一段剪贴板文字压成单行预览，空白折叠并截断到指定字符数。
fn collapsed_preview(text: &str, limit: usize) -> String {
    let mut preview = String::new();
    for (index, word) in text.split_whitespace().enumerate() {
        if index > 0 {
            preview.push(' ');
        }
        preview.push_str(word);
        if preview.chars().count() > limit {
            break;
        }
    }
    if preview.chars().count() > limit {
        let mut trimmed: String = preview.chars().take(limit.saturating_sub(1)).collect();
        trimmed.push('…');
        return trimmed;
    }
    preview
}

/// 剪贴板真的变了以后，在右下角弹一个不抢焦点的果冻提示。
/// 同一个剪贴板序号只提示一次，避免某些程序一次复制多发几次更新。
fn notify_copied(app: &tauri::AppHandle, kind: &str, preview: String, detail: String) {
    let state = app.state::<AppState>();
    let notify = match lock(&state.config) {
        Ok(config) => config.notify.clone(),
        Err(_) => return,
    };
    if !notify.enabled {
        return;
    }
    let sequence = platform::clipboard_sequence();
    if state.toast_sequence.swap(sequence, Ordering::AcqRel) == sequence {
        return;
    }
    let payload = ToastPayload {
        kind: kind.into(),
        preview,
        detail,
        duration_ms: notify.duration_ms,
    };
    let Some(toast) = app.get_webview_window("toast") else {
        return;
    };
    // 先显示窗口再发事件：卡片默认 opacity: 0，所以不会闪旧内容，
    // 而事件到达时窗口已经在屏幕上，果冻动画可以从第一帧开始播。
    if let Err(error) = platform::show_toast(&toast) {
        report(app, error);
    }
    let _ = toast.emit("toast-changed", &payload);
    let generation = state
        .toast_generation
        .fetch_add(1, Ordering::AcqRel)
        .wrapping_add(1);
    let app = app.clone();
    let duration = Duration::from_millis(u64::from(notify.duration_ms));
    let _ = std::thread::Builder::new()
        .name("toast-hide".into())
        .spawn(move || {
            std::thread::sleep(duration);
            let state = app.state::<AppState>();
            if state.toast_generation.load(Ordering::Acquire) != generation {
                return;
            }
            if let Some(toast) = app.get_webview_window("toast") {
                let _ = platform::hide_toast(&toast);
            }
        });
}

/// 验证码在历史里带的标签：既方便筛选，也让过期验证码能被批量清掉。
const PHONE_CODE_TAG: &str = "验证码";

/// 配对页上显示的名字。
const PHONE_DEVICE_NAME: &str = "剪藏";

/// 手机验证码服务的运行时状态：是否在监听、最近一次收到的验证码。
#[derive(Default)]
struct PhoneRuntime {
    running: bool,
    error: Option<String>,
    last: Option<PhoneCode>,
}

/// 最近一次收到的验证码，用于设置面板回显与倒计时。
#[derive(Clone, serde::Serialize)]
struct PhoneCode {
    code: String,
    from: Option<String>,
    received_at: u64,
    expires_at: u64,
}

/// 设置面板需要的手机接入信息：局域网地址、配对二维码与当前状态。
#[derive(Clone, serde::Serialize)]
struct PhoneStatus {
    enabled: bool,
    running: bool,
    address: String,
    token: String,
    pairing: String,
    qr: String,
    code_ttl_secs: u32,
    last: Option<PhoneCode>,
    error: Option<String>,
}

/// 只使用物理 Wi-Fi/以太网的局域网地址，不回退到代理或回环地址。
fn phone_host() -> Option<String> {
    crate::lan::lan_ipv4().map(|ip| ip.to_string())
}

/// 配对令牌为空时现场生成并写回 config.yaml，让二维码扫码即用。
fn ensure_phone_token(app: &tauri::AppHandle) -> Result<String, String> {
    let state = app.state::<AppState>();
    let mut config = lock(&state.config)?;
    if !config.phone.token.is_empty() {
        return Ok(config.phone.token.clone());
    }
    config.phone.token = relay::random_token();
    config.save(&state.config_path)?;
    Ok(config.phone.token.clone())
}

fn phone_snapshot(app: &tauri::AppHandle) -> Result<PhoneStatus, String> {
    let state = app.state::<AppState>();
    let phone = lock(&state.config)?.phone.clone();
    let host = phone_host();
    let pairing = host
        .as_deref()
        .map(|host| relay::pairing_payload(host, phone.port, &phone.token, PHONE_DEVICE_NAME))
        .unwrap_or_default();
    let qr = if phone.token.is_empty() || pairing.is_empty() {
        String::new()
    } else {
        relay::qr_data_uri(&pairing).unwrap_or_default()
    };
    let runtime = lock(&state.phone_state)?;
    Ok(PhoneStatus {
        enabled: phone.enabled,
        running: runtime.running,
        address: host.as_ref().map(|host| format!("{host}:{}", phone.port))
            .unwrap_or_else(|| "未检测到物理局域网 IPv4 地址".into()),
        token: phone.token,
        pairing,
        qr,
        code_ttl_secs: phone.code_ttl_secs,
        last: runtime.last.clone(),
        error: runtime.error.clone().or_else(|| {
            (phone.enabled && host.is_none()).then(||
                "未找到已连接的物理 Wi-Fi/以太网私有 IPv4 地址，请检查网卡和网络连接；不生成虚拟网卡配对二维码".into())
        }),
    })
}

#[tauri::command]
fn phone_status(app: tauri::AppHandle) -> Result<PhoneStatus, String> {
    phone_snapshot(&app)
}

/// 重新生成配对令牌：已经配对过的手机需要重新扫码。
#[tauri::command]
fn phone_regenerate(app: tauri::AppHandle) -> Result<PhoneStatus, String> {
    let state = app.state::<AppState>();
    let config = {
        let mut current = lock(&state.config)?;
        current.phone.token = relay::random_token();
        current.save(&state.config_path)?;
        current.clone()
    };
    sync_phone(&app);
    let _ = app.emit("settings-changed", &config);
    let _ = app.emit("phone-changed", ());
    phone_snapshot(&app)
}

/// 按当前配置起停手机验证码监听；配置变化时先停旧的，再按需起新的。
fn sync_phone(app: &tauri::AppHandle) {
    let state = app.state::<AppState>();
    // 持有生命周期锁直到新线程安装完成，防止并发设置变更交错起停。
    let mut server = match lock(&state.phone_server) {
        Ok(server) => server,
        Err(error) => return report(app, error),
    };
    drop(server.take());
    if let Ok(mut runtime) = state.phone_state.lock() {
        runtime.running = false;
        runtime.error = None;
    }
    let phone = match lock(&state.config) {
        Ok(config) => config.phone.clone(),
        Err(error) => return report(app, error),
    };
    if !phone.enabled {
        let _ = app.emit("phone-changed", ());
        return;
    }
    let token = match ensure_phone_token(app) {
        Ok(token) => token,
        Err(error) => return report(app, error),
    };
    let port = phone.port;
    let ttl = phone.code_ttl_secs;
    let context = Arc::new(RelayContext {
        token,
        version: env!("CARGO_PKG_VERSION").into(),
        host: phone_host().unwrap_or_default(),
        port,
    });
    let listener = match relay::bind(port) {
        Ok(listener) => listener,
        Err(error) => {
            let message = format!("手机验证码端口 {port} 监听失败：{error}");
            if let Ok(mut runtime) = state.phone_state.lock() {
                runtime.error = Some(message.clone());
            }
            let _ = app.emit("phone-changed", ());
            return report(app, message);
        }
    };
    if let Ok(mut runtime) = state.phone_state.lock() {
        runtime.running = true;
        runtime.error = None;
    }
    let server_app = app.clone();
    let handler_app = server_app.clone();
    let spawned = relay::RelayServer::start(
        listener,
        context,
        move |sms| on_phone_code(&handler_app, sms, ttl),
        move |result| {
            if let Err(error) = result {
                report(&server_app, format!("手机验证码监听已停止：{error}"));
            }
            let state = server_app.state::<AppState>();
            if let Ok(mut runtime) = state.phone_state.lock() {
                runtime.running = false;
            }
            let _ = server_app.emit("phone-changed", ());
        },
    );
    match spawned {
        Ok(started) => *server = Some(started),
        Err(error) => {
            if let Ok(mut runtime) = state.phone_state.lock() {
                runtime.running = false;
                runtime.error = Some(error.to_string());
            }
            report(app, error);
        }
    }
    let _ = app.emit("phone-changed", ());
}

/// 收到验证码：写进系统剪贴板、存进历史，并安排到期自动删除。
fn on_phone_code(app: &tauri::AppHandle, sms: RelaySms, ttl_secs: u32) {
    let state = app.state::<AppState>();
    let now = relay::now_seconds();
    let stored = (|| -> Result<(i64, i64), String> {
        let _gate = lock(&state.clipboard_gate)?;
        let mut clipboard = arboard::Clipboard::new().map_err(|error| error.to_string())?;
        clipboard
            .set_text(sms.code.clone())
            .map_err(|error| error.to_string())?;
        // 告诉监听线程：这次剪贴板变化是自己写的，不要再当成一次新的复制。
        state
            .own_sequence
            .store(platform::clipboard_sequence(), Ordering::Release);
        drop(clipboard);
        let mut store = lock(&state.store)?;
        let id = store.insert_text(&sms.code)?;
        store.set_entry_tags(id, &[PHONE_CODE_TAG.to_string()])?;
        let stamp = store.get(id)?.created_at;
        Ok((id, stamp))
    })();
    match stored {
        Ok((id, stamp)) => {
            if let Ok(mut runtime) = state.phone_state.lock() {
                runtime.last = Some(PhoneCode {
                    code: sms.code.clone(),
                    from: sms.from.clone(),
                    received_at: now,
                    expires_at: now.saturating_add(u64::from(ttl_secs)),
                });
                runtime.error = None;
            }
            let _ = app.emit("history-changed", ());
            let _ = app.emit("phone-changed", ());
            let from = sms.from.clone().unwrap_or_else(|| "短信".to_string());
            notify_copied(
                app,
                "code",
                sms.code.clone(),
                format!("来自 {from} · 已复制，{ttl_secs} 秒后自动清除"),
            );
            expire_code(app, id, sms.code.clone(), stamp, ttl_secs);
        }
        Err(error) => {
            if let Ok(mut runtime) = state.phone_state.lock() {
                runtime.error = Some(format!("验证码写入失败：{error}"));
            }
            let _ = app.emit("phone-changed", ());
            report(app, format!("验证码写入失败：{error}"));
        }
    }
}

/// 验证码只是一次性凭据：到期后从历史里删掉，别让明文长期留在磁盘上。
fn expire_code(app: &tauri::AppHandle, id: i64, code: String, stamp: i64, ttl_secs: u32) {
    let worker = app.clone();
    let spawned = std::thread::Builder::new()
        .name("phone-expire".into())
        .spawn(move || {
            std::thread::sleep(Duration::from_secs(u64::from(ttl_secs)));
            let state = worker.state::<AppState>();
            // 同一条记录可能已经被重新复制过（created_at 会变），那就交给新的定时器管。
            let unchanged = lock(&state.store)
                .and_then(|store| store.get(id))
                .is_ok_and(|entry| entry.text == code && entry.created_at == stamp);
            if unchanged {
                match lock(&state.store).and_then(|mut store| store.delete(id)) {
                    Ok(()) => {
                        let _ = worker.emit("history-changed", ());
                    }
                    Err(error) => report(&worker, format!("验证码自动清除失败：{error}")),
                }
            }
            if let Ok(mut runtime) = state.phone_state.lock() {
                if runtime.last.as_ref().is_some_and(|last| last.code == code) {
                    runtime.last = None;
                }
            }
            let _ = worker.emit("phone-changed", ());
        });
    if let Err(error) = spawned {
        report(app, error);
    }
}

fn report(app: &tauri::AppHandle, error: impl ToString) {
    let message = error.to_string();
    eprintln!("{message}");
    if let Some(state) = app.try_state::<AppState>() {
        if let Ok(mut last) = state.last_error.lock() {
            *last = Some(message.clone());
        }
    }
    let _ = app.emit("app-error", message);
}

fn window(app: &tauri::AppHandle) -> Result<tauri::WebviewWindow, String> {
    app.get_webview_window("main")
        .ok_or_else(|| "主窗口不存在".into())
}

fn toggle(app: &tauri::AppHandle, snippets: bool) -> Result<(), String> {
    // 单实例回调（例如用户连点两次启动图标）与全局快捷键都可能在
    // setup 里的 manage() 之前触发，此时状态还不存在；早期版本在这里
    // 直接取 state() 会 panic，而 panic 发生在 Windows 回调里无法 unwind，
    // 整个程序会以 0xc0000409 直接崩掉。
    let Some(state) = app.try_state::<AppState>() else {
        return Ok(());
    };
    if !state.ready.load(Ordering::Acquire) {
        return Ok(());
    }
    let window = window(app)?;
    if state.pasting.load(Ordering::Acquire) {
        return Ok(());
    }
    let visible = window.is_visible().map_err(|e| e.to_string())?;
    if visible && state.snippet_mode.load(Ordering::Acquire) == snippets {
        return hide_popup(app.clone());
    }
    if !visible {
        let target = platform::target_window();
        state.target.store(target, Ordering::Release);
        platform::show_popup(&window, target)?;
    }
    state.snippet_mode.store(snippets, Ordering::Release);
    window
        .emit(
            "popup-shown",
            if snippets { "snippets" } else { "clipboard" },
        )
        .map_err(|e| e.to_string())?;
    if let Some(error) = lock(&state.last_error)?.take() {
        window.emit("app-error", error).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn get_settings(state: tauri::State<AppState>) -> Result<Config, String> {
    Ok(lock(&state.config)?.clone())
}

#[tauri::command]
fn get_popup_mode(state: tauri::State<AppState>) -> &'static str {
    if state.snippet_mode.load(Ordering::Acquire) {
        "snippets"
    } else {
        "clipboard"
    }
}

#[tauri::command]
async fn list_entries(
    app: tauri::AppHandle,
    query: String,
    tag: Option<String>,
) -> Result<Vec<Entry>, String> {
    if query.len() > 4096 || tag.as_ref().is_some_and(|tag| tag.len() > 256) {
        return Err("搜索内容过长".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let limit = lock(&state.config)?.history.display_limit;
        let result = lock(&state.store)?.list(&query, tag.as_deref(), limit);
        result
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn hide_popup(app: tauri::AppHandle) -> Result<(), String> {
    clear_preview(&app)?;
    window(&app)?.hide().map_err(|e| e.to_string())
}

fn clear_preview(app: &tauri::AppHandle) -> Result<(), String> {
    let Some(state) = app.try_state::<AppState>() else {
        return Ok(());
    };
    *lock(&state.preview)? = None;
    *lock(&state.preview_terms)? = Vec::new();
    if let Some(preview) = app.get_webview_window("preview") {
        platform::hide_preview(&preview)?;
        preview
            .emit("preview-changed", ())
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn get_preview(state: tauri::State<AppState>) -> Result<PreviewPayload, String> {
    Ok(PreviewPayload {
        entry: lock(&state.preview)?.clone(),
        terms: lock(&state.preview_terms)?.clone(),
    })
}

#[tauri::command]
fn select_preview(
    app: tauri::AppHandle,
    id: Option<i64>,
    query: Option<String>,
) -> Result<(), String> {
    let main = window(&app)?;
    let state = app.state::<AppState>();
    let Some(id) = id else {
        return clear_preview(&app);
    };
    *lock(&state.preview_terms)? = preview_terms(query.as_deref().unwrap_or_default());
    if !main.is_visible().map_err(|e| e.to_string())? {
        return clear_preview(&app);
    }
    let entry = if state.snippet_mode.load(Ordering::Acquire) {
        let snippet = lock(&state.store)?.get_snippet(id)?;
        Entry {
            id: snippet.id,
            kind: "snippet".into(),
            summary: snippet.title,
            text: snippet.content,
            image_path: None,
            thumbnail_path: None,
            created_at: snippet.updated_at * 1000,
            width: None,
            height: None,
            ocr_status: "none".into(),
            ocr_error: None,
            tags: snippet.tags,
        }
    } else {
        lock(&state.store)?.get(id)?
    };
    *lock(&state.preview)? = Some(entry);
    let preview = app.get_webview_window("preview").ok_or("预览窗口不存在")?;
    preview
        .emit("preview-changed", ())
        .map_err(|e| e.to_string())?;
    platform::show_preview(&preview, &main)
}

fn parse_shortcut(value: &str) -> Result<Shortcut, String> {
    value
        .split('+')
        .map(|token| {
            if token.eq_ignore_ascii_case("win") || token.eq_ignore_ascii_case("meta") {
                "Super"
            } else {
                token
            }
        })
        .collect::<Vec<_>>()
        .join("+")
        .parse::<Shortcut>()
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn delete_entry(app: tauri::AppHandle, id: i64) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        lock(&app.state::<AppState>().store)?.delete(id)?;
        app.emit("history-changed", ()).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn clear_history(app: tauri::AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        lock(&app.state::<AppState>().store)?.clear()?;
        app.emit("history-changed", ()).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn retry_ocr(app: tauri::AppHandle, id: i64) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        lock(&state.store)?.retry_ocr(id)?;
        let _ = state.ocr_wake.try_send(());
        app.emit("history-changed", ()).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn open_config(state: tauri::State<AppState>) -> Result<(), String> {
    std::process::Command::new("notepad.exe")
        .arg(&state.config_path)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("无法打开配置：{e}"))
}

fn shortcuts(config: &Config) -> Result<[Shortcut; 2], String> {
    let clipboard = parse_shortcut(&config.hotkeys.toggle)?;
    let snippets = parse_shortcut(&config.hotkeys.snippets)?;
    if clipboard == snippets {
        return Err("剪贴板和代码片段不能使用同一个呼出快捷键".into());
    }
    Ok([clipboard, snippets])
}

#[tauri::command]
fn reload_config(app: tauri::AppHandle) -> Result<Config, String> {
    let next = Config::load(&app.state::<AppState>().config_path)?;
    apply_config(&app, next, false)
}

/// 在设置面板中直接修改配置：先校验并应用，成功后写回 YAML（保留注释）。
#[tauri::command]
fn save_settings(app: tauri::AppHandle, config: Config) -> Result<Config, String> {
    config.validate()?;
    apply_config(&app, config, true)
}

fn apply_config(app: &tauri::AppHandle, next: Config, persist: bool) -> Result<Config, String> {
    let app = app.clone();
    let state = app.state::<AppState>();
    platform::validate_chord(&next.hotkeys.paste)?;
    let new = shortcuts(&next)?;
    let mut current = lock(&state.config)?;
    let old = shortcuts(&current)?;
    let mut added = Vec::new();
    for shortcut in new.iter().filter(|s| !old.contains(s)) {
        if let Err(error) = app.global_shortcut().register(*shortcut) {
            for registered in added {
                let _ = app.global_shortcut().unregister(registered);
            }
            return Err(format!("快捷键注册失败，保留旧配置：{error}"));
        }
        added.push(*shortcut);
    }
    let apply = (|| {
        window(&app)?
            .set_size(tauri::LogicalSize::new(
                next.appearance.width,
                next.appearance.height,
            ))
            .map_err(|e| e.to_string())?;
        if persist {
            next.save(&state.config_path)?;
        }
        lock(&state.store)?.set_max_items(next.history.max_items)?;
        for shortcut in old.iter().filter(|s| !new.contains(s)) {
            app.global_shortcut()
                .unregister(*shortcut)
                .map_err(|e| e.to_string())?;
        }
        Ok::<_, String>(())
    })();
    if let Err(error) = apply {
        for registered in added {
            let _ = app.global_shortcut().unregister(registered);
        }
        for shortcut in old {
            if !app.global_shortcut().is_registered(shortcut) {
                let _ = app.global_shortcut().register(shortcut);
            }
        }
        let _ = window(&app)?.set_size(tauri::LogicalSize::new(
            current.appearance.width,
            current.appearance.height,
        ));
        return Err(error);
    }
    *current = next.clone();
    drop(current);
    sync_phone(&app);
    app.emit("settings-changed", &next)
        .map_err(|e| e.to_string())?;
    app.emit("history-changed", ()).map_err(|e| e.to_string())?;
    app.emit("snippets-changed", ())
        .map_err(|e| e.to_string())?;
    Ok(next)
}

#[tauri::command]
fn list_snippets(
    state: tauri::State<AppState>,
    query: String,
    tag: Option<String>,
) -> Result<Vec<Snippet>, String> {
    if query.len() > 4096 || tag.as_ref().is_some_and(|tag| tag.len() > 256) {
        return Err("搜索内容过长".into());
    }
    let limit = lock(&state.config)?.history.display_limit;
    let result = lock(&state.store)?.list_snippets(&query, tag.as_deref(), limit);
    result
}

#[tauri::command]
fn get_snippet(state: tauri::State<AppState>, id: i64) -> Result<Snippet, String> {
    lock(&state.store)?.get_snippet(id)
}

#[tauri::command]
fn save_snippet(
    app: tauri::AppHandle,
    id: Option<i64>,
    title: String,
    content: String,
    tags: Option<Vec<String>>,
) -> Result<i64, String> {
    let id = lock(&app.state::<AppState>().store)?.upsert_snippet(
        id,
        &title,
        &content,
        &tags.unwrap_or_default(),
    )?;
    app.emit("snippets-changed", ())
        .map_err(|e| e.to_string())?;
    Ok(id)
}

#[tauri::command]
fn set_entry_tags(
    app: tauri::AppHandle,
    id: i64,
    tags: Vec<String>,
) -> Result<Vec<String>, String> {
    let tags = lock(&app.state::<AppState>().store)?.set_entry_tags(id, &tags)?;
    app.emit("history-changed", ()).map_err(|e| e.to_string())?;
    Ok(tags)
}

#[tauri::command]
fn list_tags(state: tauri::State<AppState>, snippets: bool) -> Result<Vec<TagCount>, String> {
    lock(&state.store)?.list_tags(snippets)
}

#[tauri::command]
fn delete_snippet(app: tauri::AppHandle, id: i64) -> Result<(), String> {
    lock(&app.state::<AppState>().store)?.delete_snippet(id)?;
    app.emit("snippets-changed", ()).map_err(|e| e.to_string())
}

struct PasteGuard<'a>(&'a AtomicBool);
impl Drop for PasteGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[tauri::command]
async fn paste_entry(app: tauri::AppHandle, id: i64) -> Result<(), String> {
    paste_item(app, id, false).await
}

#[tauri::command]
async fn paste_snippet(app: tauri::AppHandle, id: i64) -> Result<(), String> {
    paste_item(app, id, true).await
}

async fn paste_item(app: tauri::AppHandle, id: i64, snippet: bool) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        if state.pasting.swap(true, Ordering::AcqRel) {
            return Err("正在粘贴，请稍候".into());
        }
        let _busy = PasteGuard(&state.pasting);
        let target = state.target.load(Ordering::Acquire);
        if target == 0 {
            return Err("请先在目标程序内按快捷键呼出剪藏".into());
        }
        let (text, image_path) = if snippet {
            (lock(&state.store)?.get_snippet(id)?.content, None)
        } else {
            let entry = lock(&state.store)?.get(id)?;
            (entry.text, entry.image_path)
        };
        let chord = lock(&state.config)?.hotkeys.paste.clone();
        let result = (|| {
            let _gate = lock(&state.clipboard_gate)?;
            let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
            if let Some(path) = image_path {
                let pixels = image::open(path).map_err(|e| e.to_string())?.into_rgba8();
                clipboard
                    .set_image(arboard::ImageData {
                        width: pixels.width() as usize,
                        height: pixels.height() as usize,
                        bytes: Cow::Owned(pixels.into_raw()),
                    })
                    .map_err(|e| e.to_string())?;
            } else {
                clipboard.set_text(text).map_err(|e| e.to_string())?;
            }
            state
                .own_sequence
                .store(platform::clipboard_sequence(), Ordering::Release);
            drop(clipboard);
            hide_popup(app.clone())?;
            platform::paste_to(target, &chord)?;
            if !snippet {
                lock(&state.store)?.touch(id)?;
            }
            app.emit(
                if snippet {
                    "snippets-changed"
                } else {
                    "history-changed"
                },
                (),
            )
            .map_err(|e| e.to_string())
        })();
        if result.is_err() {
            // Restore the picker on failure, never inject into an unrelated foreground app.
            if let Ok(popup) = window(&app) {
                let _ = popup.show();
                let _ = popup.set_focus();
            }
        }
        result
    })
    .await
    .map_err(|e| e.to_string())?
}

fn capture(app: &tauri::AppHandle, notify: bool) -> Result<(), String> {
    let state = app.state::<AppState>();
    // 自己粘贴引起的变化不提示，否则粘贴完立刻又弹出“已复制”。
    let notify = notify && !state.pasting.load(Ordering::Acquire);
    let _gate = lock(&state.clipboard_gate)?;
    let sequence = platform::clipboard_sequence();
    if sequence == state.own_sequence.load(Ordering::Acquire) {
        return Ok(());
    }
    let config = lock(&state.config)?.clone();
    let max_image = config.history.max_image_mb as usize * 1024 * 1024;
    platform::check_clipboard_size(max_image, config.history.max_text_kb as usize * 1024)?;
    let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    let image = match clipboard.get_image() {
        Ok(image) => Some(image),
        Err(arboard::Error::ContentNotAvailable) => platform::read_dib_image(max_image)?,
        Err(error) => return Err(error.to_string()),
    };
    if let Some(image) = image {
        if image.bytes.len() > max_image {
            return Err("图片超过配置的解码大小上限，未保存".into());
        }
        if platform::clipboard_sequence() != sequence {
            return Ok(());
        }
        lock(&state.store)?.insert_image(&image.bytes, image.width as u32, image.height as u32)?;
        let _ = state.ocr_wake.try_send(());
        if notify {
            notify_copied(
                app,
                "image",
                String::new(),
                format!("{} × {} 像素", image.width, image.height),
            );
        }
    } else {
        match clipboard.get_text() {
            Ok(text) if !text.is_empty() => {
                if text.len() > config.history.max_text_kb as usize * 1024 {
                    return Err("文字超过配置大小上限，未保存".into());
                }
                if platform::clipboard_sequence() != sequence {
                    return Ok(());
                }
                lock(&state.store)?.insert_text(&text)?;
                if notify {
                    let detail = format!("{} 个字符", text.chars().count());
                    notify_copied(app, "text", collapsed_preview(&text, 96), detail);
                }
            }
            Ok(_) | Err(arboard::Error::ContentNotAvailable) => return Ok(()),
            Err(error) => return Err(error.to_string()),
        }
    }
    app.emit("history-changed", ()).map_err(|e| e.to_string())
}

fn start_workers(
    app: &tauri::AppHandle,
    ocr_rx: std::sync::mpsc::Receiver<()>,
) -> Result<(), String> {
    let (capture_tx, capture_rx) = sync_channel(1);
    platform::start_listener(app.clone(), move || {
        let _ = capture_tx.try_send(());
    })?;
    let capture_app = app.clone();
    std::thread::Builder::new()
        .name("clipboard-capture".into())
        .spawn(move || {
            // Capture the current clipboard once, then sleep until WM_CLIPBOARDUPDATE.
            // 启动时的初剪贴板属于“旧内容”，不弹出提示。
            if let Err(error) = capture(&capture_app, false) {
                report(&capture_app, error);
            }
            while capture_rx.recv().is_ok() {
                let mut last = None;
                for delay in [0, 20, 60, 120] {
                    if delay > 0 {
                        std::thread::sleep(Duration::from_millis(delay));
                    }
                    match capture(&capture_app, true) {
                        Ok(()) => {
                            last = None;
                            break;
                        }
                        Err(error) => last = Some(error),
                    }
                }
                if let Some(error) = last {
                    report(&capture_app, error);
                }
            }
        })
        .map_err(|e| e.to_string())?;
    let ocr_app = app.clone();
    std::thread::Builder::new()
        .name("local-ocr".into())
        .spawn(move || {
            loop {
                let state = ocr_app.state::<AppState>();
                let pending = lock(&state.store).and_then(|store| store.pending_image());
                match pending {
                    Ok(Some(entry)) => {
                        let language = match lock(&state.config) {
                            Ok(config) => config.ocr.language.clone(),
                            Err(error) => {
                                report(&ocr_app, error);
                                break;
                            }
                        };
                        let result = entry
                            .image_path
                            .as_ref()
                            .ok_or_else(|| "图片路径缺失".to_string())
                            .and_then(|path| {
                                platform::recognize(std::path::Path::new(path), &language)
                            });
                        let (text, status, error) = match result {
                            Ok(text) if text.trim().is_empty() => (text, "empty", None),
                            Ok(text) => (text, "ready", None),
                            Err(error) => {
                                report(&ocr_app, format!("图片 OCR 失败：{error}"));
                                (String::new(), "error", Some(error))
                            }
                        };
                        // A user may delete/prune the item while OCR is running. Never recreate it.
                        let update = lock(&state.store).and_then(|mut store| {
                            if store.get(entry.id).is_err() {
                                return Ok(());
                            }
                            store.update_ocr(entry.id, &text, status, error.as_deref())
                        });
                        if let Err(error) = update {
                            report(&ocr_app, error);
                            break;
                        }
                        let _ = ocr_app.emit("history-changed", ());
                    }
                    Ok(None) => {
                        if ocr_rx.recv().is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        report(&ocr_app, error);
                        break;
                    }
                }
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn run() {
    let result = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Err(error) = toggle(app, false) {
                report(app, error);
            }
        }))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    if event.state() != ShortcutState::Pressed {
                        return;
                    }
                    // 同上：快捷键可能先于 manage() 触发，拿不到状态就忽略。
                    let Some(state) = app.try_state::<AppState>() else {
                        return;
                    };
                    if !state.ready.load(Ordering::Acquire) {
                        return;
                    }
                    let result = (|| {
                        let snippets = shortcuts(&*lock(&state.config)?)?[1] == *shortcut;
                        toggle(app, snippets)
                    })();
                    if let Err(error) = result {
                        report(app, error);
                    }
                })
                .build(),
        )
        .invoke_handler(tauri::generate_handler![
            get_settings,
            get_popup_mode,
            get_preview,
            select_preview,
            list_snippets,
            get_snippet,
            save_snippet,
            delete_snippet,
            paste_snippet,
            list_entries,
            paste_entry,
            hide_popup,
            delete_entry,
            clear_history,
            retry_ocr,
            open_config,
            reload_config,
            save_settings,
            set_entry_tags,
            list_tags,
            phone_status,
            phone_regenerate
        ])
        .setup(|app| {
            let dir = app.path().app_local_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let config_path = dir.join("config.yaml");
            let config = Config::load(&config_path)?;
            platform::validate_chord(&config.hotkeys.paste)?;
            let store = Store::open(&dir, config.history.max_items)?;
            let (ocr_wake, ocr_rx) = sync_channel(1);
            app.manage(AppState {
                ready: AtomicBool::new(false),
                config: Mutex::new(config.clone()),
                store: Mutex::new(store),
                config_path,
                target: AtomicIsize::new(0),
                own_sequence: AtomicU32::new(u32::MAX),
                clipboard_gate: Mutex::new(()),
                pasting: AtomicBool::new(false),
                ocr_wake,
                last_error: Mutex::new(None),
                snippet_mode: AtomicBool::new(false),
                preview: Mutex::new(None),
                preview_terms: Mutex::new(Vec::new()),
                toast_sequence: AtomicU32::new(u32::MAX),
                toast_generation: AtomicU32::new(0),
                phone_server: Mutex::new(None),
                phone_state: Mutex::new(PhoneRuntime::default()),
            });
            // 配置窗口使用 create:false：Tauri 默认在用户 setup 前创建窗口，
            // WebView 的 IPC/原生事件可能重入并读取尚未 manage 的 AppState。
            // 必须先注册状态，再创建任何 WebView（不能靠延时碰运气）。
            for config in app.config().app.windows.clone() {
                tauri::WebviewWindowBuilder::from_config(app.handle(), &config)?.build()?;
            }
            let popup = window(app.handle())?;
            popup.set_size(tauri::LogicalSize::new(
                config.appearance.width,
                config.appearance.height,
            ))?;
            for shortcut in shortcuts(&config)? {
                app.global_shortcut().register(shortcut)?;
            }
            let quit = tauri::menu::MenuItem::with_id(app, "quit", "退出剪藏", true, None::<&str>)?;
            let settings = tauri::menu::MenuItem::with_id(
                app,
                "settings",
                "编辑 YAML 配置",
                true,
                None::<&str>,
            )?;
            let reload =
                tauri::menu::MenuItem::with_id(app, "reload", "重新加载配置", true, None::<&str>)?;
            let menu = tauri::menu::Menu::with_items(app, &[&settings, &reload, &quit])?;
            tauri::tray::TrayIconBuilder::new()
                .icon(tauri::image::Image::new_owned(
                    include_bytes!("../icons/tray.rgba").to_vec(),
                    32,
                    32,
                ))
                .tooltip(format!("剪藏 · {}", config.hotkeys.toggle))
                .menu(&menu)
                .on_menu_event(|app, event| {
                    let result = match event.id.as_ref() {
                        "quit" => {
                            app.exit(0);
                            Ok(())
                        }
                        "settings" => open_config(app.state()),
                        "reload" => reload_config(app.clone()).map(|_| ()),
                        _ => Ok(()),
                    };
                    if let Err(error) = result {
                        report(app, error);
                    }
                })
                .build(app)?;
            start_workers(app.handle(), ocr_rx)?;
            // 上次运行留下的验证码要补扫一次：进程重启会丢掉内存里的定时器。
            let state = app.state::<AppState>();
            let ttl = config.phone.code_ttl_secs;
            if let Err(error) =
                lock(&state.store).and_then(|mut store| store.sweep_codes(PHONE_CODE_TAG, ttl))
            {
                report(app.handle(), error);
            }
            sync_phone(app.handle());
            state.ready.store(true, Ordering::Release);
            Ok(())
        })
        .on_window_event(|window, event| {
            // 复制提示不属于可交互的 picker；其焦点/关闭事件不应关闭面板。
            // 初始化前的原生事件也不能进入依赖 AppState 的路径。
            if !matches!(window.label(), "main" | "preview") {
                return;
            }
            let Some(state) = window.app_handle().try_state::<AppState>() else {
                return;
            };
            if !state.ready.load(Ordering::Acquire) {
                return;
            }
            match event {
                tauri::WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    let _ = hide_popup(window.app_handle().clone());
                }
                tauri::WindowEvent::Focused(false) => {
                    let app = window.app_handle().clone();
                    tauri::async_runtime::spawn_blocking(move || {
                        std::thread::sleep(Duration::from_millis(100));
                        let foreground = platform::target_window();
                        let inside = ["main", "preview"].iter().any(|label| {
                            app.get_webview_window(label)
                                .and_then(|w| w.hwnd().ok())
                                .is_some_and(|hwnd| hwnd.0 as isize == foreground)
                        });
                        if !inside {
                            let _ = hide_popup(app);
                        }
                    });
                }
                _ => {}
            }
        })
        .run(tauri::generate_context!());
    if let Err(error) = result {
        use windows::{
            core::HSTRING,
            Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK},
        };
        unsafe {
            MessageBoxW(
                None,
                &HSTRING::from(format!("剪藏启动失败：{error}")),
                &HSTRING::from("剪藏"),
                MB_OK | MB_ICONERROR,
            );
        }
        std::process::exit(1);
    }
}
