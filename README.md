# 剪藏（Wincpl）

Windows 11 本地剪贴板与代码片段工具。数据只保存在本机，使用 Rust + Tauri 2 构建，支持文本、图片、即时搜索、全局快捷键、Windows 毛玻璃窗口和 Windows.Media.Ocr 离线 OCR。

项目地址：<https://github.com/anlen123/wincpl>

## 功能

- `Ctrl+Alt+V` 呼出剪贴板面板，默认显示最新 20 条记录。
- 支持文本和图片；图片保留原图，并生成缩略图。
- 呼出后直接输入即可即时搜索，不需要额外按搜索键。
- 使用 Windows 本地 OCR 提取图片文字，OCR 文本可参与搜索。
- 上下方向键选择，回车或鼠标点击粘贴。
- `Ctrl+Alt+S` 呼出独立代码片段面板。
- 代码片段支持新增、编辑、删除、标题搜索、完整正文粘贴和 `Ctrl+Enter` 保存。
- 快捷键、粘贴按键、颜色、字体、字号、窗口大小和历史限制均可通过 YAML 配置。
- 系统托盘驻留，不占用任务栏。
- SQLite 持久化；重复复制的相同内容会去重并置顶。

## 安装

### 使用 GitHub Actions 构建包

仓库的 Windows 工作流会在 Windows runner 上运行测试并构建 NSIS 安装包。

1. 打开仓库的 **Actions** 页面。
2. 选择 **Windows build**，点击 **Run workflow**，或推送代码触发构建。
3. 打开完成的 workflow run，在 **Artifacts** 下载 `jianzang-windows-x64`。
4. 在 Windows 11 上运行其中的 NSIS `.exe` 安装包。

本仓库只提交源码和构建配置，不提交 `.exe`、`.dll`、安装包或其他构建产物。

### 从源码构建安装包

要求：

- Windows 10/11 本机，推荐 Windows 11
- Node.js 22.12+（或更新的 LTS 版本）
- Rust stable，MSVC toolchain
- Visual Studio 2022 Build Tools，安装“使用 C++ 的桌面开发”工作负载
- Windows 10/11 SDK
- WebView2 Runtime；Windows 11 通常已经安装

在 PowerShell 中执行：

```powershell
git clone git@github.com:anlen123/wincpl.git
cd wincpl
powershell -ExecutionPolicy Bypass -File .\build-windows.ps1
```

安装包输出目录：

```text
src-tauri\target\release\bundle\nsis\
```

也可以直接执行：

```powershell
npm ci
npm run tauri build
```

### 开发运行

在 Windows PowerShell 中执行：

```powershell
powershell -ExecutionPolicy Bypass -File .\build-windows.ps1 -Dev
```

或：

```powershell
npm ci
npm run tauri dev
```

仅运行前端预览：

```powershell
npm run dev
```

前端开发服务器默认地址为 `http://127.0.0.1:1420`。浏览器预览不具备 Windows 系统剪贴板监听、全局快捷键、焦点恢复、原生 OCR 和真实粘贴能力。

## 配置

首次运行后生成：

```text
%LOCALAPPDATA%\com.winctl.jianzang\config.yaml
```

仓库中的 [`config.example.yaml`](config.example.yaml) 是完整配置示例。可以从应用设置或托盘菜单打开配置文件，修改后选择重新加载配置。

常用配置：

```yaml
hotkeys:
  toggle: "Ctrl+Alt+V"
  snippets: "Ctrl+Alt+S"
  paste: "Shift+Insert"

history:
  display_limit: 20
  max_items: 100
  max_image_mb: 64
  max_text_kb: 256

ocr:
  language: "zh-Hans"
```

以上为常用字段节选，完整配置还包含 `appearance` 等字段。请在生成的配置上修改，或以完整示例为基础，不要用本节片段覆盖整个文件。

快捷键格式支持 `Ctrl`、`Alt`、`Shift`、`Super` 与一个字母、数字或功能键。两个呼出快捷键必须不同。系统保留的 `Win+V` 可能无法注册，程序不会强行接管它。

OCR 使用 Windows 对应语言包。若中文识别不可用，请在 Windows 设置的“时间和语言 → 语言和区域 → 对应语言 → 语言选项”中安装 OCR 语言包。

## 数据位置与隐私

应用数据位于：

```text
%LOCALAPPDATA%\com.winctl.jianzang\
```

其中包含：

- `history.sqlite3`：文本、图片元数据、OCR 文本和代码片段
- `images\`：原始图片
- `thumbnails\`：列表缩略图
- `config.yaml`：本机配置

剪贴板数据不会上传到网络。历史未加密，可能包含密码、令牌和其他敏感信息。共享电脑不建议使用，或应在设置中定期清理历史。

## 架构

- 前端：TypeScript、Vite、原生 HTML/CSS
- 桌面容器：Tauri 2
- 后端：Rust
- 存储：SQLite（`rusqlite`）
- 剪贴板：`arboard`，Windows 原生消息监听用于变更通知
- OCR：Windows.Media.Ocr，离线处理
- 窗口：Tauri Windows acrylic effect，透明无边框置顶窗口
- 粘贴：恢复原目标窗口后发送配置的键盘组合

后台监听线程只处理剪贴板变更通知；图片保存和 OCR 采用串行队列，并受配置中的数量和大小限制。WebView2 是独立进程，实际内存占用应合计所有相关进程。

## 测试与构建检查

在 Rust 工具链已安装的环境中运行：

```powershell
cargo test --manifest-path src-tauri/Cargo.toml
npm ci
npm run build
npm run tauri build
```

GitHub Actions 会在 Windows runner 上执行 Rust 测试和 Tauri 构建。Windows 原生快捷键、焦点恢复、毛玻璃、OCR 语言包、管理员权限窗口以及不同应用的粘贴兼容性，必须在真实 Windows 环境中验证。

## 权限与已知限制

- 普通权限程序通常不能向管理员权限程序注入粘贴按键；请让两个程序使用相同权限，或手动粘贴。
- 无文字图片只能得到 OCR 空结果和尺寸摘要，程序不会把照片自动识别成“人物”“风景”等场景标签。
- 多显示器和不同 DPI 缩放下，窗口定位需要在目标设备上验证。
- WebView2 Runtime 是运行时依赖。
- 当前构建未做代码签名；SmartScreen 可能警告，先前 GNU 构建也曾被 Defender 隔离。请核对来源与安全扫描结果，不建议关闭系统防护。
- GNU 交叉编译的便携版本还需要同目录的 `WebView2Loader.dll`；推荐 Windows MSVC 构建的 NSIS 安装包。
- WSL/Linux 只适合进行前端和部分 Rust 检查，不能替代 Windows 桌面端测试。

## 目录结构

```text
.
├── src/                    # 前端 TypeScript 与样式
├── index.html              # Tauri 前端入口
├── src-tauri/
│   ├── src/                # Rust 应用、存储、配置、OCR 与 Windows 集成
│   ├── icons/              # 应用图标源文件
│   ├── capabilities/       # Tauri 权限配置
│   ├── Cargo.toml
│   └── tauri.conf.json
├── config.example.yaml     # 配置示例
├── build-windows.ps1       # Windows 开发/构建脚本
└── .github/workflows/      # GitHub Actions Windows 构建
```
