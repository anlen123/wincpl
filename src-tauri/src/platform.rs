use std::{
    borrow::Cow,
    ffi::c_void,
    mem::size_of,
    panic::{catch_unwind, AssertUnwindSafe},
    path::Path,
    sync::mpsc::sync_channel,
    thread,
    time::{Duration, Instant},
};

use windows::{
    core::{w, HSTRING},
    Globalization::Language,
    Graphics::Imaging::{
        BitmapAlphaMode, BitmapDecoder, BitmapInterpolationMode, BitmapPixelFormat,
        BitmapTransform, ColorManagementMode, ExifOrientationMode,
    },
    Media::Ocr::OcrEngine,
    Storage::{FileAccessMode, StorageFile},
    Win32::{
        Foundation::{HGLOBAL, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM},
        Graphics::Gdi::{
            ClientToScreen, GetMonitorInfoW, MonitorFromPoint, MONITORINFO,
            MONITOR_DEFAULTTONEAREST,
        },
        System::{
            DataExchange::{
                AddClipboardFormatListener, CloseClipboard, GetClipboardData,
                GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard,
                RegisterClipboardFormatW, RemoveClipboardFormatListener,
            },
            LibraryLoader::GetModuleHandleW,
            Memory::{GlobalLock, GlobalSize, GlobalUnlock},
            WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED},
        },
        UI::{
            Input::KeyboardAndMouse::{
                GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
                KEYBD_EVENT_FLAGS, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_ADD,
                VK_BACK, VK_CONTROL, VK_DECIMAL, VK_DELETE, VK_DIVIDE, VK_DOWN, VK_END, VK_ESCAPE,
                VK_HOME, VK_INSERT, VK_LCONTROL, VK_LEFT, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU,
                VK_MULTIPLY, VK_NEXT, VK_NUMPAD0, VK_PRIOR, VK_RCONTROL, VK_RETURN, VK_RIGHT,
                VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SHIFT, VK_SPACE, VK_SUBTRACT, VK_TAB, VK_UP,
            },
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos,
                GetForegroundWindow, GetGUIThreadInfo, GetMessageW, GetWindowLongPtrW,
                GetWindowThreadProcessId, IsIconic, IsWindow, PostQuitMessage, RegisterClassW,
                SetForegroundWindow, SetWindowLongPtrW, ShowWindowAsync, TranslateMessage,
                CREATESTRUCTW, GUITHREADINFO, GWLP_USERDATA, HWND_MESSAGE, MSG, SW_RESTORE,
                WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLIPBOARDUPDATE, WM_DESTROY, WM_NCCREATE,
                WM_NCDESTROY, WNDCLASSW,
            },
        },
    },
};

const CF_DIB_FORMAT: u32 = 8;
const CF_UNICODETEXT_FORMAT: u32 = 13;
const CF_DIBV5_FORMAT: u32 = 17;
const BI_RGB_VALUE: u32 = 0;
const LISTENER_CLASS: windows::core::PCWSTR = w!("JianzangClipboardListenerWindow");

type ClipboardCallback = Box<dyn Fn() + Send + 'static>;

unsafe extern "system" fn clipboard_window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        let create = lparam.0 as *const CREATESTRUCTW;
        if !create.is_null() {
            let callback = unsafe { (*create).lpCreateParams } as isize;
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, callback) };
        }
        return LRESULT(1);
    }

    if message == WM_CLIPBOARDUPDATE {
        let callback =
            unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const ClipboardCallback;
        if !callback.is_null() {
            // No panic may cross the Win32 callback boundary.
            let _ = catch_unwind(AssertUnwindSafe(|| unsafe { (&*callback)() }));
        }
        return LRESULT(0);
    }

    if message == WM_DESTROY {
        unsafe { PostQuitMessage(0) };
        return LRESULT(0);
    }

    if message == WM_NCDESTROY {
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
    }

    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

fn listener_thread<F>(on_change: F, ready: std::sync::mpsc::SyncSender<Result<(), String>>)
where
    F: Fn() + Send + 'static,
{
    let result = (|| -> Result<(), String> {
        let module = unsafe { GetModuleHandleW(None) }
            .map_err(|error| format!("无法取得程序模块句柄：{error}"))?;
        let instance = HINSTANCE(module.0);
        let class = WNDCLASSW {
            lpfnWndProc: Some(clipboard_window_proc),
            hInstance: instance,
            lpszClassName: LISTENER_CLASS,
            ..Default::default()
        };
        if unsafe { RegisterClassW(&class) } == 0 {
            return Err(format!(
                "无法注册剪贴板监听窗口：{}",
                windows::core::Error::from_win32()
            ));
        }

        let callback: Box<ClipboardCallback> = Box::new(Box::new(on_change));
        let callback_ptr = Box::into_raw(callback);
        let window = match unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                LISTENER_CLASS,
                w!(""),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                Some(instance),
                Some(callback_ptr.cast::<c_void>() as *const c_void),
            )
        } {
            Ok(window) => window,
            Err(error) => {
                unsafe { drop(Box::from_raw(callback_ptr)) };
                return Err(format!("无法创建剪贴板监听窗口：{error}"));
            }
        };

        if let Err(error) = unsafe { AddClipboardFormatListener(window) } {
            unsafe {
                SetWindowLongPtrW(window, GWLP_USERDATA, 0);
                let _ = DestroyWindow(window);
                drop(Box::from_raw(callback_ptr));
            }
            return Err(format!("无法订阅剪贴板更新：{error}"));
        }

        if ready.send(Ok(())).is_err() {
            unsafe {
                let _ = RemoveClipboardFormatListener(window);
                SetWindowLongPtrW(window, GWLP_USERDATA, 0);
                let _ = DestroyWindow(window);
                drop(Box::from_raw(callback_ptr));
            }
            return Ok(());
        }

        let mut message = MSG::default();
        loop {
            let status = unsafe { GetMessageW(&mut message, None, 0, 0) }.0;
            if status <= 0 {
                break;
            }
            unsafe {
                let _translated = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }

        unsafe {
            let _ = RemoveClipboardFormatListener(window);
            SetWindowLongPtrW(window, GWLP_USERDATA, 0);
            if IsWindow(Some(window)).as_bool() {
                let _ = DestroyWindow(window);
            }
            drop(Box::from_raw(callback_ptr));
        }
        Ok(())
    })();

    if let Err(error) = result {
        let _ = ready.send(Err(error));
    }
}

pub fn start_listener<F>(_app: tauri::AppHandle, on_change: F) -> Result<(), String>
where
    F: Fn() + Send + 'static,
{
    let (ready_tx, ready_rx) = sync_channel(1);
    thread::Builder::new()
        .name("clipboard-listener".into())
        .spawn(move || listener_thread(on_change, ready_tx))
        .map_err(|error| format!("无法启动剪贴板监听线程：{error}"))?;
    ready_rx
        .recv()
        .map_err(|_| "剪贴板监听线程在启动完成前意外退出".to_string())?
}

pub fn clipboard_sequence() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}

pub fn target_window() -> isize {
    unsafe { GetForegroundWindow().0 as isize }
}

fn target_hwnd(target: isize) -> HWND {
    HWND(target as *mut c_void)
}

fn anchor_point(target: HWND) -> POINT {
    if !target.is_invalid() {
        let thread_id = unsafe { GetWindowThreadProcessId(target, None) };
        if thread_id != 0 {
            let mut info = GUITHREADINFO {
                cbSize: size_of::<GUITHREADINFO>() as u32,
                ..Default::default()
            };
            if unsafe { GetGUIThreadInfo(thread_id, &mut info) }.is_ok()
                && !info.hwndCaret.is_invalid()
            {
                let mut point = POINT {
                    x: info.rcCaret.left,
                    y: info.rcCaret.bottom,
                };
                if unsafe { ClientToScreen(info.hwndCaret, &mut point) }.as_bool() {
                    return point;
                }
            }
        }
    }

    let mut point = POINT::default();
    if unsafe { GetCursorPos(&mut point) }.is_err() {
        point = POINT { x: 0, y: 0 };
    }
    point
}

pub fn show_popup(window: &tauri::WebviewWindow, target: isize) -> Result<(), String> {
    let anchor = anchor_point(target_hwnd(target));
    let monitor = unsafe { MonitorFromPoint(anchor, MONITOR_DEFAULTTONEAREST) };
    if monitor.is_invalid() {
        return Err("无法确定光标所在显示器".into());
    }
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        return Err(format!(
            "无法读取显示器工作区：{}",
            windows::core::Error::from_win32()
        ));
    }

    let size = window.outer_size().map_err(|error| error.to_string())?;
    let width = i32::try_from(size.width).unwrap_or(i32::MAX);
    let height = i32::try_from(size.height).unwrap_or(i32::MAX);
    let work = info.rcWork;
    let gap = 10;
    let preferred_x = anchor.x.saturating_add(gap);
    let below_y = anchor.y.saturating_add(gap);
    let above_y = anchor.y.saturating_sub(height).saturating_sub(gap);
    let preferred_y = if below_y.saturating_add(height) <= work.bottom {
        below_y
    } else {
        above_y
    };
    let max_x = work.right.saturating_sub(width).max(work.left);
    let max_y = work.bottom.saturating_sub(height).max(work.top);
    let x = preferred_x.clamp(work.left, max_x);
    let y = preferred_y.clamp(work.top, max_y);

    window
        .set_position(tauri::PhysicalPosition::new(x, y))
        .map_err(|error| format!("无法定位剪藏窗口：{error}"))?;
    window
        .show()
        .map_err(|error| format!("无法显示剪藏窗口：{error}"))?;
    window
        .set_focus()
        .map_err(|error| format!("无法聚焦剪藏窗口：{error}"))
}

pub fn show_preview(
    preview: &tauri::WebviewWindow,
    main: &tauri::WebviewWindow,
) -> Result<(), String> {
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, ShowWindow, HWND_TOPMOST, SWP_NOACTIVATE, SW_SHOWNOACTIVATE,
    };
    let position = main.outer_position().map_err(|e| e.to_string())?;
    let monitor = unsafe {
        MonitorFromPoint(
            POINT {
                x: position.x,
                y: position.y,
            },
            MONITOR_DEFAULTTONEAREST,
        )
    };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        return Err("无法读取预览显示器工作区".into());
    }
    let scale = main.scale_factor().map_err(|e| e.to_string())?;
    let gap = (16.0 * scale).round() as i32;
    let work = info.rcWork;
    let width = ((480.0 * scale).round() as i32).min((work.right - work.left - 2 * gap).max(1));
    let height = ((560.0 * scale).round() as i32).min((work.bottom - work.top - 2 * gap).max(1));
    let hwnd = preview.hwnd().map_err(|e| e.to_string())?;
    unsafe {
        SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            work.right - width - gap,
            work.bottom - height - gap,
            width,
            height,
            SWP_NOACTIVATE,
        )
        .map_err(|e| e.to_string())?;
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
    Ok(())
}

pub fn hide_preview(preview: &tauri::WebviewWindow) -> Result<(), String> {
    use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};
    let hwnd = preview.hwnd().map_err(|e| e.to_string())?;
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct ParsedChord {
    modifiers: [VIRTUAL_KEY; 4],
    modifier_count: usize,
    key: VIRTUAL_KEY,
    key_extended: bool,
}

fn named_key(token: &str) -> Option<(VIRTUAL_KEY, bool)> {
    let key = if token.eq_ignore_ascii_case("insert") || token.eq_ignore_ascii_case("ins") {
        (VK_INSERT, true)
    } else if token.eq_ignore_ascii_case("enter") || token.eq_ignore_ascii_case("return") {
        (VK_RETURN, false)
    } else if token.eq_ignore_ascii_case("tab") {
        (VK_TAB, false)
    } else if token.eq_ignore_ascii_case("escape") || token.eq_ignore_ascii_case("esc") {
        (VK_ESCAPE, false)
    } else if token.eq_ignore_ascii_case("space") {
        (VK_SPACE, false)
    } else if token.eq_ignore_ascii_case("backspace") || token.eq_ignore_ascii_case("back") {
        (VK_BACK, false)
    } else if token.eq_ignore_ascii_case("delete") || token.eq_ignore_ascii_case("del") {
        (VK_DELETE, true)
    } else if token.eq_ignore_ascii_case("home") {
        (VK_HOME, true)
    } else if token.eq_ignore_ascii_case("end") {
        (VK_END, true)
    } else if token.eq_ignore_ascii_case("pageup") || token.eq_ignore_ascii_case("pgup") {
        (VK_PRIOR, true)
    } else if token.eq_ignore_ascii_case("pagedown") || token.eq_ignore_ascii_case("pgdn") {
        (VK_NEXT, true)
    } else if token.eq_ignore_ascii_case("left") {
        (VK_LEFT, true)
    } else if token.eq_ignore_ascii_case("right") {
        (VK_RIGHT, true)
    } else if token.eq_ignore_ascii_case("up") {
        (VK_UP, true)
    } else if token.eq_ignore_ascii_case("down") {
        (VK_DOWN, true)
    } else if token.eq_ignore_ascii_case("add") {
        (VK_ADD, false)
    } else if token.eq_ignore_ascii_case("subtract") {
        (VK_SUBTRACT, false)
    } else if token.eq_ignore_ascii_case("multiply") {
        (VK_MULTIPLY, false)
    } else if token.eq_ignore_ascii_case("divide") {
        (VK_DIVIDE, true)
    } else if token.eq_ignore_ascii_case("decimal") {
        (VK_DECIMAL, false)
    } else if token.len() >= 2 && matches!(token.as_bytes()[0], b'F' | b'f') {
        let number = token[1..].parse::<u16>().ok()?;
        if !(1..=24).contains(&number) {
            return None;
        }
        (VIRTUAL_KEY(0x70 + number - 1), false)
    } else if token.len() == 7
        && token
            .as_bytes()
            .get(..6)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"numpad"))
    {
        let digit = token.as_bytes()[6];
        if !digit.is_ascii_digit() {
            return None;
        }
        (VIRTUAL_KEY(VK_NUMPAD0.0 + u16::from(digit - b'0')), false)
    } else if token.len() == 1 {
        let byte = token.as_bytes()[0];
        if byte.is_ascii_alphabetic() {
            (VIRTUAL_KEY(u16::from(byte.to_ascii_uppercase())), false)
        } else if byte.is_ascii_digit() {
            (VIRTUAL_KEY(u16::from(byte)), false)
        } else {
            return None;
        }
    } else {
        return None;
    };
    Some(key)
}

fn parse_chord(chord: &str) -> Result<ParsedChord, String> {
    let mut parsed = ParsedChord {
        modifiers: [VIRTUAL_KEY(0); 4],
        modifier_count: 0,
        key: VIRTUAL_KEY(0),
        key_extended: false,
    };

    for raw in chord.split('+') {
        let token = raw.trim();
        if token.is_empty() {
            return Err("粘贴按键组合包含空按键".into());
        }
        let modifier =
            if token.eq_ignore_ascii_case("ctrl") || token.eq_ignore_ascii_case("control") {
                Some(VK_CONTROL)
            } else if token.eq_ignore_ascii_case("alt") {
                Some(VK_MENU)
            } else if token.eq_ignore_ascii_case("shift") {
                Some(VK_SHIFT)
            } else if token.eq_ignore_ascii_case("win")
                || token.eq_ignore_ascii_case("windows")
                || token.eq_ignore_ascii_case("super")
                || token.eq_ignore_ascii_case("meta")
            {
                Some(VK_LWIN)
            } else {
                None
            };

        if let Some(modifier) = modifier {
            if parsed.modifiers[..parsed.modifier_count].contains(&modifier) {
                return Err(format!("粘贴按键组合包含重复修饰键：{token}"));
            }
            if parsed.modifier_count == parsed.modifiers.len() {
                return Err("粘贴按键组合包含过多修饰键".into());
            }
            parsed.modifiers[parsed.modifier_count] = modifier;
            parsed.modifier_count += 1;
            continue;
        }

        if parsed.key.0 != 0 {
            return Err("粘贴按键组合只能包含一个普通按键".into());
        }
        let (key, extended) =
            named_key(token).ok_or_else(|| format!("不支持的粘贴按键：{token}"))?;
        parsed.key = key;
        parsed.key_extended = extended;
    }

    if parsed.key.0 == 0 {
        return Err("粘贴按键组合缺少普通按键".into());
    }
    Ok(parsed)
}

pub fn validate_chord(chord: &str) -> Result<(), String> {
    parse_chord(chord).map(|_| ())
}

fn key_input(key: VIRTUAL_KEY, extended: bool, released: bool) -> INPUT {
    let mut flags = KEYBD_EVENT_FLAGS(0);
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if released {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: key,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn physical_modifiers_released() -> bool {
    const MODIFIERS: [VIRTUAL_KEY; 8] = [
        VK_LCONTROL,
        VK_RCONTROL,
        VK_LMENU,
        VK_RMENU,
        VK_LSHIFT,
        VK_RSHIFT,
        VK_LWIN,
        VK_RWIN,
    ];
    MODIFIERS
        .iter()
        .all(|key| unsafe { GetAsyncKeyState(i32::from(key.0)) } >= 0)
}

pub fn paste_to(target: isize, chord: &str) -> Result<(), String> {
    let parsed = parse_chord(chord)?;
    if target == 0 {
        return Err("没有可粘贴的目标窗口".into());
    }
    let target = target_hwnd(target);
    if !unsafe { IsWindow(Some(target)) }.as_bool() {
        return Err("原目标窗口已经关闭，已取消粘贴".into());
    }

    if unsafe { IsIconic(target) }.as_bool() {
        if !unsafe { ShowWindowAsync(target, SW_RESTORE) }.as_bool() {
            return Err("无法恢复已最小化的目标窗口".into());
        }
    }
    let _requested_focus = unsafe { SetForegroundWindow(target) };

    let focus_deadline = Instant::now() + Duration::from_millis(350);
    while unsafe { GetForegroundWindow() } != target && Instant::now() < focus_deadline {
        thread::sleep(Duration::from_millis(10));
    }
    if unsafe { GetForegroundWindow() } != target {
        return Err("无法安全地恢复原目标窗口，已取消粘贴".into());
    }

    let release_deadline = Instant::now() + Duration::from_millis(600);
    while !physical_modifiers_released() && Instant::now() < release_deadline {
        thread::sleep(Duration::from_millis(5));
    }
    if !physical_modifiers_released() {
        return Err("快捷键修饰键尚未释放，已取消粘贴".into());
    }
    if unsafe { GetForegroundWindow() } != target {
        return Err("焦点已切换到其他窗口，已取消粘贴".into());
    }

    let mut inputs = [INPUT::default(); 10];
    let mut count = 0;
    for &modifier in &parsed.modifiers[..parsed.modifier_count] {
        inputs[count] = key_input(modifier, modifier == VK_LWIN, false);
        count += 1;
    }
    inputs[count] = key_input(parsed.key, parsed.key_extended, false);
    count += 1;
    inputs[count] = key_input(parsed.key, parsed.key_extended, true);
    count += 1;
    for &modifier in parsed.modifiers[..parsed.modifier_count].iter().rev() {
        inputs[count] = key_input(modifier, modifier == VK_LWIN, true);
        count += 1;
    }

    let sent = unsafe { SendInput(&inputs[..count], size_of::<INPUT>() as i32) };
    if sent != count as u32 {
        return Err(format!(
            "系统只发送了 {sent}/{count} 个粘贴按键事件：{}",
            windows::core::Error::from_win32()
        ));
    }
    Ok(())
}

struct RoApartment;

impl RoApartment {
    fn initialize() -> Result<Self, String> {
        unsafe { RoInitialize(RO_INIT_MULTITHREADED) }
            .map_err(|error| format!("无法初始化 Windows Runtime：{error}"))?;
        Ok(Self)
    }
}

impl Drop for RoApartment {
    fn drop(&mut self) {
        unsafe { RoUninitialize() };
    }
}

fn available_ocr_languages() -> Result<String, String> {
    let languages = OcrEngine::AvailableRecognizerLanguages()
        .map_err(|error| format!("无法读取 OCR 语言列表：{error}"))?;
    let count = languages
        .Size()
        .map_err(|error| format!("无法读取 OCR 语言数量：{error}"))?;
    let mut tags = Vec::with_capacity(count as usize);
    for index in 0..count {
        let language = languages
            .GetAt(index)
            .map_err(|error| format!("无法读取 OCR 语言：{error}"))?;
        tags.push(
            language
                .LanguageTag()
                .map_err(|error| format!("无法读取 OCR 语言标记：{error}"))?
                .to_string(),
        );
    }
    Ok(if tags.is_empty() {
        "无".into()
    } else {
        tags.join(", ")
    })
}

pub fn recognize(path: &Path, language: &str) -> Result<String, String> {
    let _apartment = RoApartment::initialize()?;
    let tag = language.trim();
    if tag.is_empty() {
        return Err("OCR language 不能为空，请填写已安装的语言标记（例如 zh-CN）".into());
    }
    let tag = HSTRING::from(tag);
    if !Language::IsWellFormed(&tag).map_err(|error| format!("无法校验 OCR 语言：{error}"))?
    {
        return Err(format!("OCR 语言标记无效：{}", language.trim()));
    }
    let language_object = Language::CreateLanguage(&tag)
        .map_err(|error| format!("无法创建 OCR 语言 {}：{error}", language.trim()))?;
    if !OcrEngine::IsLanguageSupported(&language_object)
        .map_err(|error| format!("无法检查 OCR 语言支持：{error}"))?
    {
        return Err(format!(
            "未安装 OCR 语言包 {}；当前可用：{}",
            language.trim(),
            available_ocr_languages()?
        ));
    }
    let engine = OcrEngine::TryCreateFromLanguage(&language_object)
        .map_err(|error| format!("无法创建 {} OCR 引擎：{error}", language.trim()))?;

    let absolute = path
        .canonicalize()
        .map_err(|error| format!("无法定位 OCR 图片 {}：{error}", path.display()))?;
    // WinRT StorageFile expects a DOS/UNC path, not canonicalize's extended prefix.
    let canonical_text = absolute
        .to_str()
        .ok_or_else(|| "OCR 图片路径包含 Windows Runtime 无法表示的字符".to_string())?;
    let path_text = if let Some(unc) = canonical_text.strip_prefix("\\\\?\\UNC\\") {
        format!("\\\\{unc}")
    } else {
        canonical_text
            .strip_prefix("\\\\?\\")
            .unwrap_or(canonical_text)
            .to_owned()
    };
    let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(path_text))
        .and_then(|operation| operation.get())
        .map_err(|error| format!("无法打开 OCR 图片 {}：{error}", absolute.display()))?;
    let stream = file
        .OpenAsync(FileAccessMode::Read)
        .and_then(|operation| operation.get())
        .map_err(|error| format!("无法读取 OCR 图片 {}：{error}", absolute.display()))?;
    let decoder = BitmapDecoder::CreateAsync(&stream)
        .and_then(|operation| operation.get())
        .map_err(|error| format!("无法解码 OCR 图片 {}：{error}", absolute.display()))?;
    let width = decoder
        .PixelWidth()
        .map_err(|error| format!("无法读取 OCR 图片宽度：{error}"))?;
    let height = decoder
        .PixelHeight()
        .map_err(|error| format!("无法读取 OCR 图片高度：{error}"))?;
    if width == 0 || height == 0 {
        return Err("OCR 图片尺寸无效".into());
    }

    let max_dimension = OcrEngine::MaxImageDimension()
        .map_err(|error| format!("无法读取 OCR 图片尺寸上限：{error}"))?;
    if max_dimension == 0 {
        return Err("系统报告的 OCR 图片尺寸上限无效".into());
    }
    let largest = width.max(height);
    let (scaled_width, scaled_height) = if largest > max_dimension {
        let scale = |value: u32| {
            ((u64::from(value) * u64::from(max_dimension)) / u64::from(largest)).max(1) as u32
        };
        (scale(width), scale(height))
    } else {
        (width, height)
    };
    let recognize_at = |width, height| -> Result<String, String> {
        let transform =
            BitmapTransform::new().map_err(|error| format!("无法创建 OCR 图片缩放器：{error}"))?;
        transform
            .SetScaledWidth(width)
            .and_then(|_| transform.SetScaledHeight(height))
            .and_then(|_| transform.SetInterpolationMode(BitmapInterpolationMode::Fant))
            .map_err(|error| format!("无法设置 OCR 图片缩放参数：{error}"))?;
        let bitmap = decoder
            .GetSoftwareBitmapTransformedAsync(
                BitmapPixelFormat::Bgra8,
                BitmapAlphaMode::Ignore,
                &transform,
                ExifOrientationMode::RespectExifOrientation,
                ColorManagementMode::DoNotColorManage,
            )
            .and_then(|operation| operation.get())
            .map_err(|error| format!("无法准备 OCR 图片：{error}"))?;
        let result = engine
            .RecognizeAsync(&bitmap)
            .and_then(|operation| operation.get())
            .and_then(|result| result.Text())
            .map(|text| crate::ocr::normalize_text(&text.to_string()))
            .map_err(|error| format!("Windows 本地 OCR 识别失败：{error}"));
        let _ = bitmap.Close();
        result
    };
    let text = recognize_at(scaled_width, scaled_height)?;
    // Small screenshots can be entirely missed at native resolution. Retry only
    // empty results, doubling dimensions without exceeding the engine's limit.
    if text.is_empty() && largest <= max_dimension / 2 {
        return recognize_at(width * 2, height * 2);
    }
    Ok(text)
}

struct ClipboardGuard;

impl ClipboardGuard {
    fn open() -> Result<Self, String> {
        unsafe { OpenClipboard(None) }.map_err(|error| format!("无法打开剪贴板：{error}"))?;
        Ok(Self)
    }
}

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        let _ = unsafe { CloseClipboard() };
    }
}

struct GlobalMemory {
    handle: HGLOBAL,
    pointer: *const u8,
    size: usize,
}

impl GlobalMemory {
    unsafe fn from_clipboard(format: u32) -> Result<Self, String> {
        let handle = unsafe { GetClipboardData(format) }
            .map_err(|error| format!("无法读取剪贴板格式 {format}：{error}"))?;
        let handle = HGLOBAL(handle.0);
        let size = unsafe { GlobalSize(handle) };
        if size == 0 {
            return Err(format!("剪贴板格式 {format} 的数据大小无效"));
        }
        let pointer = unsafe { GlobalLock(handle) } as *const u8;
        if pointer.is_null() {
            return Err(format!(
                "无法锁定剪贴板格式 {format}：{}",
                windows::core::Error::from_win32()
            ));
        }
        Ok(Self {
            handle,
            pointer,
            size,
        })
    }

    fn bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.pointer, self.size) }
    }
}

impl Drop for GlobalMemory {
    fn drop(&mut self) {
        // GlobalUnlock returns false both for failure and when the lock count reaches zero.
        let _ = unsafe { GlobalUnlock(self.handle) };
    }
}

fn dib_dimensions(bytes: &[u8]) -> Result<(u32, u32, u16, u32, usize), String> {
    const HEADER_SIZE: usize = 40;
    if bytes.len() < HEADER_SIZE {
        return Err("剪贴板 DIB 头部不完整".into());
    }
    let header_size = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
    if header_size < HEADER_SIZE || header_size > bytes.len() {
        return Err("剪贴板 DIB 头部大小无效".into());
    }
    let signed_width = i32::from_le_bytes(bytes[4..8].try_into().unwrap());
    let signed_height = i32::from_le_bytes(bytes[8..12].try_into().unwrap());
    let planes = u16::from_le_bytes(bytes[12..14].try_into().unwrap());
    let bit_count = u16::from_le_bytes(bytes[14..16].try_into().unwrap());
    let compression = u32::from_le_bytes(bytes[16..20].try_into().unwrap());
    let colors_used = u32::from_le_bytes(bytes[32..36].try_into().unwrap()) as usize;
    if signed_width <= 0 || signed_height == 0 || planes != 1 {
        return Err("剪贴板 DIB 尺寸或颜色平面无效".into());
    }
    let width = signed_width as u32;
    let height = signed_height.unsigned_abs();
    let palette_bytes = colors_used
        .checked_mul(4)
        .ok_or_else(|| "剪贴板 DIB 调色板大小溢出".to_string())?;
    let pixel_offset = header_size
        .checked_add(palette_bytes)
        .ok_or_else(|| "剪贴板 DIB 像素偏移溢出".to_string())?;
    Ok((width, height, bit_count, compression, pixel_offset))
}

fn decoded_rgba_size(width: u32, height: u32) -> Result<usize, String> {
    (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "剪贴板图片尺寸溢出".to_string())
}

fn ensure_image_limit(width: u32, height: u32, max_bytes: usize) -> Result<(), String> {
    let bytes = decoded_rgba_size(width, height)?;
    if bytes > max_bytes {
        return Err(format!(
            "剪贴板图片解码后需要 {bytes} 字节，超过配置上限 {max_bytes} 字节"
        ));
    }
    Ok(())
}

fn check_png(bytes: &[u8], max_bytes: usize) -> Result<(), String> {
    const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if bytes.len() < 24 || &bytes[..8] != PNG_SIGNATURE || &bytes[12..16] != b"IHDR" {
        return Err("剪贴板 PNG 头部无效".into());
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
    if width == 0 || height == 0 {
        return Err("剪贴板 PNG 尺寸无效".into());
    }
    ensure_image_limit(width, height, max_bytes)
}

pub fn check_clipboard_size(max_image_bytes: usize, max_text_bytes: usize) -> Result<(), String> {
    let _clipboard = ClipboardGuard::open()?;
    let png_format = unsafe { RegisterClipboardFormatW(w!("PNG")) };
    if png_format != 0 && unsafe { IsClipboardFormatAvailable(png_format) }.is_ok() {
        let memory = unsafe { GlobalMemory::from_clipboard(png_format) }?;
        if memory.size > max_image_bytes {
            return Err(format!(
                "剪贴板 PNG 数据为 {} 字节，超过配置上限 {max_image_bytes} 字节",
                memory.size
            ));
        }
        return check_png(memory.bytes(), max_image_bytes);
    }

    for format in [CF_DIBV5_FORMAT, CF_DIB_FORMAT] {
        if unsafe { IsClipboardFormatAvailable(format) }.is_ok() {
            let memory = unsafe { GlobalMemory::from_clipboard(format) }?;
            let (width, height, _, _, _) = dib_dimensions(memory.bytes())?;
            return ensure_image_limit(width, height, max_image_bytes);
        }
    }

    if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT_FORMAT) }.is_ok() {
        let memory = unsafe { GlobalMemory::from_clipboard(CF_UNICODETEXT_FORMAT) }?;
        if memory.size % 2 != 0 {
            return Err("剪贴板 Unicode 文本长度无效".into());
        }
        let units = unsafe {
            std::slice::from_raw_parts(memory.pointer.cast::<u16>(), memory.size / size_of::<u16>())
        };
        let end = units
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(units.len());
        let mut utf8_bytes = 0usize;
        for character in char::decode_utf16(units[..end].iter().copied()) {
            let character = character.map_err(|_| "剪贴板包含无效 UTF-16 文本".to_string())?;
            utf8_bytes = utf8_bytes
                .checked_add(character.len_utf8())
                .ok_or_else(|| "剪贴板文本长度溢出".to_string())?;
            if utf8_bytes > max_text_bytes {
                return Err(format!(
                    "剪贴板文字为 {utf8_bytes} 字节，超过配置上限 {max_text_bytes} 字节"
                ));
            }
        }
    }
    Ok(())
}

pub fn read_dib_image(max_bytes: usize) -> Result<Option<arboard::ImageData<'static>>, String> {
    let _clipboard = ClipboardGuard::open()?;
    if unsafe { IsClipboardFormatAvailable(CF_DIB_FORMAT) }.is_err() {
        return Ok(None);
    }
    let memory = unsafe { GlobalMemory::from_clipboard(CF_DIB_FORMAT) }?;
    let bytes = memory.bytes();
    let (width, height, bit_count, compression, pixel_offset) = dib_dimensions(bytes)?;
    if compression != BI_RGB_VALUE {
        return Err(format!("不支持压缩方式为 {compression} 的传统 CF_DIB 图片"));
    }
    if bit_count != 24 && bit_count != 32 {
        return Err(format!("不支持 {bit_count} 位的传统 CF_DIB 图片"));
    }
    let output_size = decoded_rgba_size(width, height)?;
    if output_size > max_bytes {
        return Err(format!(
            "剪贴板图片解码后需要 {output_size} 字节，超过配置上限 {max_bytes} 字节"
        ));
    }
    let bits_per_row = (width as usize)
        .checked_mul(bit_count as usize)
        .ok_or_else(|| "剪贴板 DIB 行宽溢出".to_string())?;
    let stride = bits_per_row
        .checked_add(31)
        .map(|value| (value / 32) * 4)
        .ok_or_else(|| "剪贴板 DIB 行跨度溢出".to_string())?;
    let pixel_bytes = stride
        .checked_mul(height as usize)
        .ok_or_else(|| "剪贴板 DIB 像素长度溢出".to_string())?;
    let end = pixel_offset
        .checked_add(pixel_bytes)
        .ok_or_else(|| "剪贴板 DIB 数据长度溢出".to_string())?;
    if end > bytes.len() {
        return Err("剪贴板 DIB 像素数据不完整".into());
    }

    let top_down = i32::from_le_bytes(bytes[8..12].try_into().unwrap()) < 0;
    let bytes_per_pixel = usize::from(bit_count / 8);
    let mut rgba = vec![0u8; output_size];
    for destination_y in 0..height as usize {
        let source_y = if top_down {
            destination_y
        } else {
            height as usize - 1 - destination_y
        };
        let source_row = pixel_offset + source_y * stride;
        let destination_row = destination_y * width as usize * 4;
        for x in 0..width as usize {
            let source = source_row + x * bytes_per_pixel;
            let destination = destination_row + x * 4;
            rgba[destination] = bytes[source + 2];
            rgba[destination + 1] = bytes[source + 1];
            rgba[destination + 2] = bytes[source];
            // BI_RGB does not define an alpha channel, even at 32 bits.
            rgba[destination + 3] = 255;
        }
    }

    Ok(Some(arboard::ImageData {
        width: width as usize,
        height: height as usize,
        bytes: Cow::Owned(rgba),
    }))
}
