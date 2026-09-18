import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./style.css";

type Appearance = {
  text_color: string;
  muted_color: string;
  background_color: string;
  selected_color: string;
  selected_border_color: string;
  card_color: string;
  border_color: string;
  accent_color: string;
  font_family: string;
  font_size: number;
  width: number;
  height: number;
};

type Config = {
  hotkeys: { toggle: string; snippets: string; paste: string };
  appearance: Appearance;
  history: { display_limit: number; max_items: number; max_image_mb: number; max_text_kb: number };
  ocr: { language: string };
};

type Entry = {
  id: number;
  kind: string;
  text: string;
  summary: string;
  image_path: string | null;
  thumbnail_path: string | null;
  created_at: number;
  width: number | null;
  height: number | null;
  ocr_status: string;
  ocr_error: string | null;
};
type Snippet = { id: number; title: string; content: string; updated_at: number };
let mode: "clipboard" | "snippets" = "clipboard";
let editingId: number | null = null;
let originalDraft = "";
let savingSnippet = false;

const requireElement = <T extends HTMLElement>(id: string): T => {
  const element = document.getElementById(id);
  if (!element) throw new Error(`缺少界面元素：${id}`);
  return element as T;
};

const root = requireElement<HTMLElement>("app");
const searchInput = requireElement<HTMLInputElement>("history-search");
const historyRegion = requireElement<HTMLElement>("history-region");
const historyList = requireElement<HTMLUListElement>("history-list");
const stateView = requireElement<HTMLElement>("state-view");
const resultCount = requireElement<HTMLElement>("result-count");
const searchStatus = requireElement<HTMLElement>("search-status");
const errorBanner = requireElement<HTMLElement>("error-banner");
const errorMessage = requireElement<HTMLElement>("error-message");
const dismissError = requireElement<HTMLButtonElement>("dismiss-error");
const settingsToggle = requireElement<HTMLButtonElement>("settings-toggle");
const settingsPanel = requireElement<HTMLElement>("settings-panel");
const settingsScrim = requireElement<HTMLElement>("settings-scrim");
const settingsClose = requireElement<HTMLButtonElement>("settings-close");
const toggleShortcut = requireElement<HTMLElement>("toggle-shortcut");
const pasteShortcut = requireElement<HTMLElement>("paste-shortcut");
const openConfigButton = requireElement<HTMLButtonElement>("open-config");
const reloadConfigButton = requireElement<HTMLButtonElement>("reload-config");
const deleteSelectedButton = requireElement<HTMLButtonElement>("delete-selected");
const clearHistoryButton = requireElement<HTMLButtonElement>("clear-history");
const nativeNote = requireElement<HTMLElement>("native-note");
const retryOcrButton = requireElement<HTMLButtonElement>("retry-ocr");
const ocrDetail = requireElement<HTMLElement>("ocr-detail");
const snippetTools = requireElement<HTMLElement>("snippet-tools");
const snippetEditor = requireElement<HTMLDialogElement>("snippet-editor");
const snippetTitle = requireElement<HTMLInputElement>("snippet-title");
const snippetContent = requireElement<HTMLTextAreaElement>("snippet-content");
const snippetSave = requireElement<HTMLButtonElement>("snippet-save");
const snippetEdit = requireElement<HTMLButtonElement>("snippet-edit");
const snippetDelete = requireElement<HTMLButtonElement>("snippet-delete");

const hasNativeBackend = "__TAURI_INTERNALS__" in window;
const dateFormatter = new Intl.DateTimeFormat("zh-CN", {
  month: "numeric",
  day: "numeric",
  hour: "2-digit",
  minute: "2-digit",
});

let entries: Entry[] = [];
let selectedId: number | null = null;
let displayedQuery = "";
let requestVersion = 0;
let searchTimer: number | undefined;
let composing = false;
let pasteInFlight = false;
let panelOpen = false;
let historyRefreshPending = false;
let resetSelectionPending = true;

let previewQueue: Promise<unknown> = Promise.resolve();
let previewRevision = 0;

function syncPreview(id: number | null): void {
  if (!hasNativeBackend) return;
  const revision = ++previewRevision;
  previewQueue = previewQueue.then(async () => {
    if (revision === previewRevision) await invoke("select_preview", { id });
  }).catch(showError);
}
const clearNode = (node: Element): void => node.replaceChildren();

function errorText(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return "发生了未知错误";
}

function showError(error: unknown): void {
  errorMessage.textContent = errorText(error);
  errorBanner.hidden = false;
}

function clearError(): void {
  errorBanner.hidden = true;
  errorMessage.textContent = "";
}

function setState(title: string, detail: string, mode: "loading" | "empty" | "error" | "unavailable"): void {
  clearNode(stateView);
  stateView.dataset.mode = mode;
  if (mode === "loading") {
    const spinner = document.createElement("span");
    spinner.className = "spinner";
    spinner.setAttribute("aria-hidden", "true");
    stateView.append(spinner);
  } else {
    const glyph = document.createElement("span");
    glyph.className = "state-glyph";
    glyph.setAttribute("aria-hidden", "true");
    glyph.textContent = mode === "unavailable" ? "桌" : mode === "error" ? "!" : "○";
    stateView.append(glyph);
  }
  const heading = document.createElement("strong");
  heading.textContent = title;
  const paragraph = document.createElement("p");
  paragraph.textContent = detail;
  stateView.append(heading, paragraph);
  stateView.hidden = false;
  historyList.hidden = true;
  syncPreview(null);
}

function applySettings(config: Config): void {
  const style = document.documentElement.style;
  const appearance = config.appearance;
  style.setProperty("--text-color", appearance.text_color);
  style.setProperty("--muted-color", appearance.muted_color);
  style.setProperty("--background-color", appearance.background_color);
  style.setProperty("--selected-color", appearance.selected_color);
  style.setProperty("--selected-border-color", appearance.selected_border_color);
  style.setProperty("--card-color", appearance.card_color);
  style.setProperty("--border-color", appearance.border_color);
  style.setProperty("--accent-color", appearance.accent_color);
  style.setProperty("--ui-font", appearance.font_family);
  style.setProperty("--font-size", `${appearance.font_size}px`);
  style.setProperty("--configured-width", `${appearance.width}px`);
  style.setProperty("--configured-height", `${appearance.height}px`);
  toggleShortcut.textContent = config.hotkeys.toggle.replace(/Super|Meta/gi, "Win");
  pasteShortcut.textContent = config.hotkeys.paste;
  requireElement("snippet-shortcut").textContent = config.hotkeys.snippets;
}

function formatTimestamp(timestamp: number): string {
  const milliseconds = timestamp < 10_000_000_000 ? timestamp * 1000 : timestamp;
  const date = new Date(milliseconds);
  return Number.isNaN(date.getTime()) ? "时间未知" : dateFormatter.format(date);
}

function ocrLabel(status: string): { label: string; className: string } | null {
  switch (status) {
    case "pending":
      return { label: "正在本地识别", className: "pending" };
    case "ready":
      return { label: "图片文字已提取", className: "ready" };
    case "empty":
      return { label: "未识别到文字", className: "empty" };
    case "error":
      return { label: "文字识别失败", className: "error" };
    default:
      return null;
  }
}

function entryTitle(entry: Entry): string {
  if (entry.kind === "image") return entry.summary || entry.text || "图片";
  return entry.text || entry.summary || "空白文字";
}

function makeEntryCard(entry: Entry): HTMLLIElement {
  const card = document.createElement("li");
  card.id = `entry-option-${entry.id}`;
  card.className = "history-card";
  card.dataset.entryId = String(entry.id);
  card.setAttribute("role", "option");
  card.setAttribute("aria-label", `${entry.kind === "image" ? "图片" : "文字"}，${entryTitle(entry)}`);
  card.title = entry.ocr_error || entryTitle(entry);
  const marker = document.createElement("span");
  marker.className = "selection-marker";
  marker.setAttribute("aria-hidden", "true");
  const thumbnail = document.createElement("div");
  thumbnail.className = "thumbnail";
  thumbnail.textContent = entry.kind === "image" ? "图" : "文";
  const path = entry.thumbnail_path;
  if (entry.kind === "image" && path) {
    const image = document.createElement("img");
    image.src = convertFileSrc(path);
    image.alt = "剪贴板图片缩略图";
    image.loading = "lazy";
    image.addEventListener("error", () => { thumbnail.textContent = "图"; }, { once: true });
    thumbnail.replaceChildren(image);
  }
  const content = document.createElement("div");
  content.className = "entry-content";
  const text = document.createElement("p");
  text.className = "entry-text";
  text.textContent = entryTitle(entry);
  content.append(text);
  const status = entry.kind === "image" ? ocrLabel(entry.ocr_status) : null;
  if (status) {
    const badge = document.createElement("span");
    badge.className = `ocr-status ${status.className}`;
    badge.textContent = status.label;
    badge.title = entry.ocr_error || status.label;
    content.append(badge);
  }
  const timestamp = document.createElement("time");
  timestamp.textContent = formatTimestamp(entry.created_at);
  card.append(marker, thumbnail, content, timestamp);
  card.addEventListener("click", (event) => {
    if (event.button !== 0) return;
    selectEntry(entry.id, false);
    void pasteSelected();
  });
  return card;
}

function renderSelection(scroll = false): void {
  for (const card of historyList.querySelectorAll<HTMLElement>(".history-card")) {
    const isSelected = Number(card.dataset.entryId) === selectedId;
    card.classList.toggle("selected", isSelected);
    card.setAttribute("aria-selected", String(isSelected));
    if (isSelected && scroll) card.scrollIntoView({ block: "nearest" });
  }
  if (selectedId === null) searchInput.removeAttribute("aria-activedescendant");
  else searchInput.setAttribute("aria-activedescendant", `entry-option-${selectedId}`);
  deleteSelectedButton.disabled = selectedId === null || !hasNativeBackend;
  snippetEdit.disabled = snippetDelete.disabled = selectedId === null;
  const selected = entries.find((entry) => entry.id === selectedId);
  retryOcrButton.disabled = !hasNativeBackend || selected?.kind !== "image" || selected.ocr_status === "pending";
  ocrDetail.textContent = selected?.kind === "image"
    ? selected.ocr_error || ocrLabel(selected.ocr_status)?.label || "本地图片"
    : "Windows.Media.Ocr";
  syncPreview(mode === "clipboard" && !panelOpen && !snippetEditor.open ? selectedId : null);
}

function selectEntry(id: number, scroll: boolean): void {
  if (!entries.some((entry) => entry.id === id)) return;
  selectedId = id;
  renderSelection(scroll);
}

function renderEntries(nextEntries: Entry[], preserveSelection: boolean): void {
  entries = nextEntries;
  if (!preserveSelection || !entries.some((entry) => entry.id === selectedId)) {
    selectedId = entries[0]?.id ?? null;
  }
  clearNode(historyList);
  for (const entry of entries) historyList.append(makeEntryCard(entry));
  stateView.hidden = true;
  historyList.hidden = false;
  historyRegion.setAttribute("aria-busy", "false");
  renderSelection(false);
}

function updateCount(query: string): void {
  resultCount.textContent = query
    ? `${entries.length} 条匹配记录`
    : `最近 ${entries.length} 条`;
}

async function loadEntries(options: { loading?: boolean; focusAfter?: HTMLElement } = {}): Promise<void> {
  if (!hasNativeBackend) return;
  const query = searchInput.value.trim();
  const version = ++requestVersion;
  const preserveSelection = !resetSelectionPending && query === displayedQuery;
  if (options.loading) {
    historyRegion.setAttribute("aria-busy", "true");
    setState("正在读取剪藏", "记录只在这台电脑上处理", "loading");
  } else {
    searchStatus.textContent = "搜索中…";
  }
  try {
    const nextEntries = mode === "snippets"
      ? (await invoke<Snippet[]>("list_snippets", { query })).map((s): Entry => ({id:s.id,kind:"snippet",text:s.title,summary:s.title,created_at:s.updated_at,image_path:null,thumbnail_path:null,width:null,height:null,ocr_status:"none",ocr_error:null}))
      : await invoke<Entry[]>("list_entries", { query });
    if (version !== requestVersion) return;
    displayedQuery = query;
    renderEntries(nextEntries, preserveSelection);
    if (resetSelectionPending || !preserveSelection) historyList.scrollTop = 0;
    resetSelectionPending = false;
    updateCount(query);
    searchStatus.textContent = "";
    if (nextEntries.length === 0) {
      if (mode === "snippets") setState(query ? "没有匹配的片段" : "还没有代码片段", query ? "仅搜索标题，不搜索代码正文" : "点击右上角 + 新建，保存常用代码与文本", "empty");
      else if (query) setState("没有找到", "试试更短的关键词，图片可按本地识别出的文字搜索", "empty");
      else setState("还没有剪藏", "复制文字或图片后，记录会出现在这里", "empty");
    }
    if (options.focusAfter?.isConnected) options.focusAfter.focus();
  } catch (error) {
    if (version !== requestVersion) return;
    entries = [];
    selectedId = null;
    renderSelection(false);
    historyRegion.setAttribute("aria-busy", "false");
    resultCount.textContent = "读取失败";
    searchStatus.textContent = "";
    setState("无法读取历史", errorText(error), "error");
    showError(error);
  }
}

function scheduleSearch(): void {
  window.clearTimeout(searchTimer);
  ++requestVersion;
  searchTimer = window.setTimeout(() => void loadEntries(), 80);
}

function moveSelection(direction: 1 | -1): void {
  if (entries.length === 0) return;
  const current = entries.findIndex((entry) => entry.id === selectedId);
  const start = current < 0 ? (direction === 1 ? -1 : 0) : current;
  const next = (start + direction + entries.length) % entries.length;
  const entry = entries[next];
  if (entry) selectEntry(entry.id, true);
}

async function pasteSelected(): Promise<void> {
  if (!hasNativeBackend || selectedId === null || pasteInFlight || snippetEditor.open || searchInput.value.trim() !== displayedQuery) return;
  pasteInFlight = true;
  ++previewRevision;
  root.classList.add("is-pasting");
  historyRegion.setAttribute("aria-busy", "true");
  searchStatus.textContent = "正在粘贴…";
  try {
    await invoke(mode === "snippets" ? "paste_snippet" : "paste_entry", { id: selectedId });
  } catch (error) {
    showError(error);
    searchStatus.textContent = "粘贴失败";
  } finally {
    pasteInFlight = false;
    root.classList.remove("is-pasting");
    historyRegion.setAttribute("aria-busy", "false");
    if (searchStatus.textContent === "正在粘贴…") searchStatus.textContent = "";
  }
}

async function hidePopup(): Promise<void> {
  if (!hasNativeBackend) return;
  ++previewRevision;
  try {
    await invoke("hide_popup");
  } catch (error) {
    showError(error);
  }
}

function openSettings(): void {
  panelOpen = true;
  syncPreview(null);
  settingsPanel.hidden = false;
  settingsScrim.hidden = false;
  settingsToggle.setAttribute("aria-expanded", "true");
  requestAnimationFrame(() => {
    root.classList.add("settings-open");
    settingsClose.focus();
  });
}

function closeSettings(restoreFocus = true): void {
  panelOpen = false;
  root.classList.remove("settings-open");
  settingsToggle.setAttribute("aria-expanded", "false");
  settingsPanel.hidden = true;
  settingsScrim.hidden = true;
  if (restoreFocus) settingsToggle.focus();
  renderSelection();
}

async function runConfigAction(command: "open_config" | "reload_config"): Promise<void> {
  if (!hasNativeBackend) return;
  const focusTarget = command === "reload_config" ? reloadConfigButton : openConfigButton;
  focusTarget.disabled = true;
  try {
    if (command === "reload_config") {
      const config = await invoke<Config>(command);
      applySettings(config);
      await loadEntries({ focusAfter: focusTarget });
    } else {
      await invoke(command);
      focusTarget.focus();
    }
  } catch (error) {
    showError(error);
    focusTarget.focus();
  } finally {
    focusTarget.disabled = false;
  }
}

async function deleteSelected(): Promise<void> {
  if (!hasNativeBackend || selectedId === null) return;
  deleteSelectedButton.disabled = true;
  try {
    await invoke("delete_entry", { id: selectedId });
    await loadEntries({ focusAfter: deleteSelectedButton });
    if (selectedId === null) searchInput.focus();
  } catch (error) {
    showError(error);
    deleteSelectedButton.focus();
  } finally {
    deleteSelectedButton.disabled = selectedId === null;
  }
}

async function clearHistory(): Promise<void> {
  if (!hasNativeBackend) return;
  const confirmed = window.confirm("确定清空全部剪藏记录吗？图片文件也会从本机删除，此操作无法撤销。");
  if (!confirmed) {
    clearHistoryButton.focus();
    return;
  }
  clearHistoryButton.disabled = true;
  try {
    await invoke("clear_history");
    await loadEntries({ focusAfter: clearHistoryButton });
  } catch (error) {
    showError(error);
    clearHistoryButton.focus();
  } finally {
    clearHistoryButton.disabled = false;
  }
}

function resetForPopup(nextMode: "clipboard" | "snippets" = "clipboard"): void {
  if (snippetEditor.open) { snippetTitle.focus(); return; }
  mode = nextMode;
  snippetTools.hidden = mode !== "snippets";
  requireElement("panel-title").textContent = mode === "snippets" ? "代码片段" : "剪藏";
  searchInput.placeholder = mode === "snippets" ? "搜索片段标题" : "搜索剪贴板";
  historyList.setAttribute("aria-label", mode === "snippets" ? "代码片段" : "剪贴板记录");
  searchInput.setAttribute("aria-label", searchInput.placeholder);
  requireElement("history-settings").hidden = mode === "snippets";
  requireElement("ocr-settings").hidden = mode === "snippets";
  historyRefreshPending = false;
  window.clearTimeout(searchTimer);
  searchInput.value = "";
  displayedQuery = "";
  selectedId = null;
  resetSelectionPending = true;
  historyList.scrollTop = 0;
  renderSelection();
  clearError();
  if (panelOpen) closeSettings(false);
  searchInput.focus();
  void loadEntries({ loading: true });
}

function refreshForHistoryChange(): void {
  if (document.hidden) {
    historyRefreshPending = true;
    return;
  }
  historyRefreshPending = false;
  void loadEntries();
}

function handleKeyboard(event: KeyboardEvent): void {
  if (event.isComposing || composing || event.key === "Process") return;
  if (snippetEditor.open) {
    if (event.key === "Enter" && event.ctrlKey) { event.preventDefault(); void saveSnippet(); }
    return;
  }
  if (panelOpen) {
    if (event.key === "Escape") {
      event.preventDefault();
      closeSettings();
    } else if (event.key === "Tab") {
      const focusable = Array.from(settingsPanel.querySelectorAll<HTMLElement>("button:not(:disabled), [href], input:not(:disabled), [tabindex]:not([tabindex='-1'])"));
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (first && last && event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (first && last && !event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    }
    return;
  }
  if (event.key === "Enter" && event.target instanceof HTMLElement && event.target.closest("button")) return;
  switch (event.key) {
    case "ArrowDown":
      event.preventDefault();
      moveSelection(1);
      break;
    case "ArrowUp":
      event.preventDefault();
      moveSelection(-1);
      break;
    case "Enter":
      event.preventDefault();
      void pasteSelected();
      break;
    case "Escape":
      event.preventDefault();
      void hidePopup();
      break;
  }
}

async function initializeNative(): Promise<void> {
  try {
    const config = await invoke<Config>("get_settings");
    applySettings(config);
    await Promise.all([
      listen<string>("popup-shown", (event) => resetForPopup(event.payload === "snippets" ? "snippets" : "clipboard")),
      listen("history-changed", () => { if (mode === "clipboard") refreshForHistoryChange(); }),
      listen("snippets-changed", () => { if (mode === "snippets") refreshForHistoryChange(); }),
      listen<string>("app-error", (event) => showError(event.payload)),
      listen<Config>("settings-changed", (event) => applySettings(event.payload)),
    ]);
    const popupMode = await invoke<string>("get_popup_mode");
    resetForPopup(popupMode === "snippets" ? "snippets" : "clipboard");
  } catch (error) {
    showError(error);
    setState("桌面服务未就绪", errorText(error), "error");
    resultCount.textContent = "连接失败";
  }
}

function initializeBrowserPreview(): void {
  historyRegion.setAttribute("aria-busy", "false");
  setState("仅可在 Windows 桌面应用中使用", "浏览器无法访问系统剪贴板；这里不会显示模拟记录。", "unavailable");
  resultCount.textContent = "桌面功能不可用";
  nativeNote.hidden = false;
  openConfigButton.disabled = true;
  reloadConfigButton.disabled = true;
  deleteSelectedButton.disabled = true;
  clearHistoryButton.disabled = true;
}

searchInput.addEventListener("input", () => {
  if (!composing) scheduleSearch();
});
searchInput.addEventListener("compositionstart", () => { composing = true; });
searchInput.addEventListener("compositionend", () => {
  composing = false;
  scheduleSearch();
});
document.addEventListener("keydown", handleKeyboard);
document.addEventListener("visibilitychange", () => {
  if (!document.hidden && historyRefreshPending) refreshForHistoryChange();
});
dismissError.addEventListener("click", clearError);
settingsToggle.addEventListener("click", openSettings);
settingsClose.addEventListener("click", () => closeSettings());
settingsScrim.addEventListener("click", () => closeSettings());
openConfigButton.addEventListener("click", () => void runConfigAction("open_config"));
reloadConfigButton.addEventListener("click", () => void runConfigAction("reload_config"));
retryOcrButton.addEventListener("click", async () => {
  if (selectedId === null || retryOcrButton.disabled) return;
  retryOcrButton.disabled = true;
  try {
    await invoke("retry_ocr", { id: selectedId });
    await loadEntries();
  } catch (error) { showError(error); }
  finally { renderSelection(); }
});
deleteSelectedButton.addEventListener("click", () => void deleteSelected());
clearHistoryButton.addEventListener("click", () => void clearHistory());


async function openSnippetEditor(id: number | null): Promise<void> {
  if (!hasNativeBackend || snippetEditor.open) return;
  try {
    const snippet = id === null ? null : await invoke<Snippet>("get_snippet", { id });
    editingId = id;
    snippetTitle.value = snippet?.title ?? "";
    snippetContent.value = snippet?.content ?? "";
    originalDraft = JSON.stringify([snippetTitle.value, snippetContent.value]);
    requireElement("editor-title").textContent = id === null ? "新增代码片段" : "编辑代码片段";
    requireElement("snippet-error").textContent = "";
    snippetEditor.showModal();
    snippetTitle.focus();
  } catch (error) { showError(error); }
}

function cancelSnippet(): void {
  if (savingSnippet) return;
  if (JSON.stringify([snippetTitle.value, snippetContent.value]) !== originalDraft && !window.confirm("放弃未保存的片段修改？")) return;
  snippetEditor.close();
  searchInput.focus();
}

async function saveSnippet(): Promise<void> {
  if (savingSnippet || !snippetEditor.open) return;
  if (!snippetTitle.value.trim() || !snippetContent.value) {
    requireElement("snippet-error").textContent = "请填写标题和内容"; return;
  }
  savingSnippet = true; snippetSave.disabled = true;
  try {
    await invoke("save_snippet", { id: editingId, title: snippetTitle.value, content: snippetContent.value });
    snippetEditor.close();
    resetForPopup("snippets");
  } catch (error) { requireElement("snippet-error").textContent = errorText(error); }
  finally { savingSnippet = false; snippetSave.disabled = false; }
}

requireElement("snippet-new").addEventListener("click", () => void openSnippetEditor(null));
snippetEdit.addEventListener("click", () => { if (selectedId !== null) void openSnippetEditor(selectedId); });
snippetDelete.addEventListener("click", async () => {
  if (selectedId === null || !window.confirm("确定删除所选代码片段？不可撤销。")) return;
  snippetDelete.disabled = true;
  try { await invoke("delete_snippet", { id: selectedId }); await loadEntries(); }
  catch (error) { showError(error); }
  finally { renderSelection(); }
});
snippetSave.addEventListener("click", () => void saveSnippet());
requireElement("snippet-cancel").addEventListener("click", cancelSnippet);
snippetEditor.addEventListener("cancel", (event) => { event.preventDefault(); cancelSnippet(); });

if (hasNativeBackend) void initializeNative();
else initializeBrowserPreview();
