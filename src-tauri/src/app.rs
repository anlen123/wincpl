use crate::{
    config::Config,
    platform,
    store::{Entry, Snippet, Store},
};
use std::{
    borrow::Cow,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering},
        mpsc::{sync_channel, SyncSender},
        Mutex, MutexGuard,
    },
    time::Duration,
};
use tauri::{Emitter, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

struct AppState {
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
}

fn lock<T>(value: &Mutex<T>) -> Result<MutexGuard<'_, T>, String> {
    value
        .lock()
        .map_err(|_| "内部状态锁已损坏，请重新启动剪藏".into())
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
    let window = window(app)?;
    let state = app.state::<AppState>();
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
async fn list_entries(app: tauri::AppHandle, query: String) -> Result<Vec<Entry>, String> {
    if query.len() > 4096 {
        return Err("搜索内容过长".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let limit = lock(&state.config)?.history.display_limit;
        let result = lock(&state.store)?.list(&query, limit);
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
    *lock(&app.state::<AppState>().preview)? = None;
    if let Some(preview) = app.get_webview_window("preview") {
        platform::hide_preview(&preview)?;
        preview
            .emit("preview-changed", ())
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn get_preview(state: tauri::State<AppState>) -> Result<Option<Entry>, String> {
    Ok(lock(&state.preview)?.clone())
}

#[tauri::command]
fn select_preview(app: tauri::AppHandle, id: Option<i64>) -> Result<(), String> {
    let main = window(&app)?;
    let state = app.state::<AppState>();
    if id.is_none()
        || !main.is_visible().map_err(|e| e.to_string())?
        || state.snippet_mode.load(Ordering::Acquire)
    {
        return clear_preview(&app);
    }
    let entry = lock(&state.store)?.get(id.unwrap())?;
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
    let state = app.state::<AppState>();
    let next = Config::load(&state.config_path)?;
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
    app.emit("settings-changed", &next)
        .map_err(|e| e.to_string())?;
    app.emit("history-changed", ()).map_err(|e| e.to_string())?;
    app.emit("snippets-changed", ())
        .map_err(|e| e.to_string())?;
    Ok(next)
}

#[tauri::command]
fn list_snippets(state: tauri::State<AppState>, query: String) -> Result<Vec<Snippet>, String> {
    if query.len() > 4096 {
        return Err("搜索内容过长".into());
    }
    let limit = lock(&state.config)?.history.display_limit;
    let result = lock(&state.store)?.list_snippets(&query, limit);
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
) -> Result<i64, String> {
    let id = lock(&app.state::<AppState>().store)?.upsert_snippet(id, &title, &content)?;
    app.emit("snippets-changed", ())
        .map_err(|e| e.to_string())?;
    Ok(id)
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

fn capture(app: &tauri::AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
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
            if let Err(error) = capture(&capture_app) {
                report(&capture_app, error);
            }
            while capture_rx.recv().is_ok() {
                let mut last = None;
                for delay in [0, 20, 60, 120] {
                    if delay > 0 {
                        std::thread::sleep(Duration::from_millis(delay));
                    }
                    match capture(&capture_app) {
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
                    if event.state() == ShortcutState::Pressed {
                        let result = (|| {
                            let state = app.state::<AppState>();
                            let snippets = shortcuts(&*lock(&state.config)?)?[1] == *shortcut;
                            toggle(app, snippets)
                        })();
                        if let Err(error) = result {
                            report(app, error);
                        }
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
            reload_config
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
            });
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
            Ok(())
        })
        .on_window_event(|window, event| match event {
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
