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
  tags: string[];
};
type Snippet = { id: number; title: string; content: string; updated_at: number; tags: string[] };
type TagCount = { name: string; count: number };
type Mode = "clipboard" | "snippets";

const MAX_TAGS = 12;

let mode: Mode = "clipboard";
let editingId: number | null = null;
let originalDraft = "";
let savingSnippet = false;
let savingTags = false;
let savingSettings = false;
let tagEditingId: number | null = null;
let currentConfig: Config | null = null;

const requireElement = <T extends HTMLElement>(id: string): T => {
  const element = document.getElementById(id);
  if (!element) throw new Error(`缺少界面元素：${id}`);
  return element as T;
};

const root = requireElement<HTMLElement>("app");
const panelTitle = requireElement<HTMLElement>("panel-title");
const panelMode = requireElement<HTMLElement>("panel-mode");
const searchInput = requireElement<HTMLInputElement>("history-search");
const tagBar = requireElement<HTMLElement>("tag-bar");
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
const settingsForm = requireElement<HTMLFormElement>("settings-form");
const settingsFields = requireElement<HTMLElement>("settings-fields");
const settingsStatus = requireElement<HTMLElement>("settings-status");
const settingsSave = requireElement<HTMLButtonElement>("settings-save");
const settingsRevert = requireElement<HTMLButtonElement>("settings-revert");
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
const snippetTags = requireElement<HTMLInputElement>("snippet-tags");
const snippetContent = requireElement<HTMLTextAreaElement>("snippet-content");
const snippetSave = requireElement<HTMLButtonElement>("snippet-save");
const snippetEdit = requireElement<HTMLButtonElement>("snippet-edit");
const snippetDelete = requireElement<HTMLButtonElement>("snippet-delete");
const tagEditButton = requireElement<HTMLButtonElement>("tag-edit");
const tagEditor = requireElement<HTMLDialogElement>("tag-editor");
const tagEditorTarget = requireElement<HTMLElement>("tag-editor-target");
const tagInput = requireElement<HTMLInputElement>("tag-input");
const tagSuggestions = requireElement<HTMLElement>("tag-suggestions");
const tagError = requireElement<HTMLElement>("tag-error");
const tagSave = requireElement<HTMLButtonElement>("tag-save");

const hasNativeBackend = "__TAURI_INTERNALS__" in window;
const dateFormatter = new Intl.DateTimeFormat("zh-CN", {
  month: "numeric",
  day: "numeric",
  hour: "2-digit",
  minute: "2-digit",
});

let entries: Entry[] = [];
let tagCounts: TagCount[] = [];
let activeTag: string | null = null;
let selectedId: number | null = null;
let displayedQuery = "";
let displayedTag: string | null = null;
let requestVersion = 0;
let searchTimer: number | undefined;
let composing = false;
let pasteInFlight = false;
let panelOpen = false;
let historyRefreshPending = false;
let resetSelectionPending = true;

let previewQueue: Promise<unknown> = Promise.resolve();
let previewRevision = 0;

function syncPreview(id: number | null, query: string): void {
  if (!hasNativeBackend) return;
  const revision = ++previewRevision;
  previewQueue = previewQueue.then(async () => {
    if (revision === previewRevision) await invoke("select_preview", { id, query });
  }).catch(showError);
}
const clearNode = (node: Element): void => node.replaceChildren();
const sameTag = (a: string | null, b: string | null): boolean =>
  a === b || (a !== null && b !== null && a.toLowerCase() === b.toLowerCase());
const dialogOpen = (): boolean => snippetEditor.open || tagEditor.open;

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

/** 标签输入：空格、逗号、顿号分隔；去掉开头的 #，忽略大小写去重。 */
function parseTags(value: string): string[] {
  const seen = new Set<string>();
  const tags: string[] = [];
  for (const raw of value.split(/[\s,，、;；]+/)) {
    const tag = raw.replace(/^#+/, "").trim();
    if (!tag || seen.has(tag.toLowerCase())) continue;
    seen.add(tag.toLowerCase());
    tags.push(tag);
  }
  return tags;
}

function setState(title: string, detail: string, stateMode: "loading" | "empty" | "error" | "unavailable"): void {
  clearNode(stateView);
  stateView.dataset.mode = stateMode;
  if (stateMode === "loading") {
    const spinner = document.createElement("span");
    spinner.className = "spinner";
    spinner.setAttribute("aria-hidden", "true");
    stateView.append(spinner);
  } else {
    const glyph = document.createElement("span");
    glyph.className = "state-glyph";
    glyph.setAttribute("aria-hidden", "true");
    glyph.textContent = stateMode === "unavailable" ? "桌" : stateMode === "error" ? "!" : mode === "snippets" ? "{ }" : "○";
    stateView.append(glyph);
  }
  const heading = document.createElement("strong");
  heading.textContent = title;
  const paragraph = document.createElement("p");
  paragraph.textContent = detail;
  stateView.append(heading, paragraph);
  stateView.hidden = false;
  historyList.hidden = true;
  syncPreview(null, "");
}

const displayChord = (value: string): string => value.replace(/Super|Meta/gi, "Win");

function applyAppearance(appearance: Partial<Appearance>): void {
  const style = document.documentElement.style;
  const colors: (keyof Appearance)[] = [
    "text_color", "muted_color", "background_color", "selected_color",
    "selected_border_color", "card_color", "border_color", "accent_color",
  ];
  for (const key of colors) {
    const value = appearance[key];
    if (typeof value === "string" && value.trim()) style.setProperty(`--${key.replaceAll("_", "-")}`, value);
  }
  if (appearance.font_family?.trim()) style.setProperty("--ui-font", appearance.font_family);
  if (typeof appearance.font_size === "number" && appearance.font_size >= 10 && appearance.font_size <= 32) {
    style.setProperty("--font-size", `${appearance.font_size}px`);
  }
  if (appearance.width) style.setProperty("--configured-width", `${appearance.width}px`);
  if (appearance.height) style.setProperty("--configured-height", `${appearance.height}px`);
}

function updateModeLabels(): void {
  const hotkeys = currentConfig?.hotkeys;
  panelTitle.textContent = mode === "snippets" ? "代码片段" : "剪藏";
  const chord = hotkeys ? displayChord(mode === "snippets" ? hotkeys.snippets : hotkeys.toggle) : "";
  panelMode.textContent = mode === "snippets"
    ? `片段库${chord ? ` · ${chord}` : ""}`
    : `剪贴板历史${chord ? ` · ${chord}` : ""} · 仅存本机`;
}

function applySettings(config: Config): void {
  currentConfig = config;
  applyAppearance(config.appearance);
  updateModeLabels();
}

function formatTimestamp(timestamp: number): string {
  const milliseconds = timestamp < 10_000_000_000 ? timestamp * 1000 : timestamp;
  const date = new Date(milliseconds);
  if (Number.isNaN(date.getTime())) return "时间未知";
  const now = new Date();
  if (date.toDateString() === now.toDateString()) {
    return date.toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" });
  }
  return dateFormatter.format(date);
}

function ocrLabel(status: string): { label: string; className: string } | null {
  switch (status) {
    case "pending":
      return { label: "正在本地识别", className: "pending" };
    case "ready":
      return { label: "已提取文字", className: "ready" };
    case "empty":
      return { label: "未识别到文字", className: "empty" };
    case "error":
      return { label: "识别失败", className: "error" };
    default:
      return null;
  }
}

function entryTitle(entry: Entry): string {
  if (entry.kind === "image") return entry.summary || entry.text || "图片";
  return entry.text || entry.summary || "空白文字";
}

function kindLabel(kind: string): string {
  return kind === "image" ? "图片" : kind === "snippet" ? "代码片段" : "文字";
}

function makeEntryCard(entry: Entry): HTMLLIElement {
  const card = document.createElement("li");
  card.id = `entry-option-${entry.id}`;
  card.className = `history-card kind-${entry.kind}`;
  card.dataset.entryId = String(entry.id);
  card.setAttribute("role", "option");
  const tagText = entry.tags.length ? `，标签 ${entry.tags.join("、")}` : "";
  card.setAttribute("aria-label", `${kindLabel(entry.kind)}，${entryTitle(entry)}${tagText}`);
  card.title = entry.ocr_error || entryTitle(entry).slice(0, 400);
  const marker = document.createElement("span");
  marker.className = "selection-marker";
  marker.setAttribute("aria-hidden", "true");
  const thumbnail = document.createElement("div");
  thumbnail.className = "thumbnail";
  thumbnail.setAttribute("aria-hidden", "true");
  thumbnail.textContent = entry.kind === "image" ? "图" : entry.kind === "snippet" ? "{ }" : "文";
  const path = entry.thumbnail_path;
  if (entry.kind === "image" && path) {
    const image = document.createElement("img");
    image.src = convertFileSrc(path);
    image.alt = "";
    image.loading = "lazy";
    image.addEventListener("error", () => { thumbnail.textContent = "图"; }, { once: true });
    thumbnail.replaceChildren(image);
  }
  const content = document.createElement("div");
  content.className = "entry-content";
  const text = document.createElement("p");
  text.className = "entry-text";
  text.textContent = entryTitle(entry).replace(/\s+/g, " ").slice(0, 300);
  content.append(text);

  const status = entry.kind === "image" ? ocrLabel(entry.ocr_status) : null;
  if (status || entry.tags.length) {
    const meta = document.createElement("div");
    meta.className = "entry-meta";
    if (status) {
      const badge = document.createElement("span");
      badge.className = `ocr-status ${status.className}`;
      badge.textContent = status.label;
      badge.title = entry.ocr_error || status.label;
      meta.append(badge);
    }
    for (const tag of entry.tags) {
      const chip = document.createElement("button");
      chip.type = "button";
      chip.className = "card-tag";
      chip.tabIndex = -1;
      chip.textContent = `#${tag}`;
      chip.title = `只看 #${tag}`;
      chip.classList.toggle("active", sameTag(tag, activeTag));
      chip.addEventListener("click", (event) => {
        event.stopPropagation();
        setTagFilter(sameTag(tag, activeTag) ? null : tag);
      });
      meta.append(chip);
    }
    content.append(meta);
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
  tagEditButton.disabled = selectedId === null || !hasNativeBackend;
  const selected = entries.find((entry) => entry.id === selectedId);
  retryOcrButton.disabled = !hasNativeBackend || selected?.kind !== "image" || selected.ocr_status === "pending";
  ocrDetail.textContent = selected?.kind === "image"
    ? selected.ocr_error || ocrLabel(selected.ocr_status)?.label || "本地图片"
    : "使用 Windows.Media.Ocr 离线识别图片文字，识别结果可参与搜索。";
  // 剪贴板与代码片段都在屏幕角落显示完整预览；有弹层时收起，避免遮挡。
  syncPreview(!panelOpen && !dialogOpen() ? selectedId : null, displayedQuery);
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

// ---------- 标签筛选 ----------

function tagFilterOptions(): (string | null)[] {
  const names = tagCounts.map((tag) => tag.name);
  if (activeTag && !names.some((name) => sameTag(name, activeTag))) names.unshift(activeTag);
  return [null, ...names];
}

function renderTagBar(): void {
  const options = tagFilterOptions();
  clearNode(tagBar);
  tagBar.hidden = options.length <= 1;
  if (tagBar.hidden) return;
  for (const tag of options) {
    const chip = document.createElement("button");
    chip.type = "button";
    chip.className = "filter-chip";
    chip.tabIndex = -1;
    const pressed = sameTag(tag, activeTag);
    chip.setAttribute("aria-pressed", String(pressed));
    const label = document.createElement("span");
    label.textContent = tag === null ? "全部" : `#${tag}`;
    chip.append(label);
    const count = tag === null ? null : tagCounts.find((item) => sameTag(item.name, tag))?.count ?? 0;
    if (count !== null) {
      const badge = document.createElement("small");
      badge.textContent = String(count);
      chip.append(badge);
    }
    chip.addEventListener("click", () => setTagFilter(tag));
    tagBar.append(chip);
    if (pressed) requestAnimationFrame(() => chip.scrollIntoView({ block: "nearest", inline: "nearest" }));
  }
}

function setTagFilter(tag: string | null): void {
  if (sameTag(tag, activeTag)) return;
  activeTag = tag;
  resetSelectionPending = true;
  renderTagBar();
  window.clearTimeout(searchTimer);
  void loadEntries();
  if (!panelOpen && !dialogOpen()) searchInput.focus();
}

function cycleTagFilter(direction: 1 | -1): void {
  const options = tagFilterOptions();
  if (options.length <= 1) return;
  const current = Math.max(0, options.findIndex((tag) => sameTag(tag, activeTag)));
  setTagFilter(options[(current + direction + options.length) % options.length] ?? null);
}

async function loadTags(): Promise<TagCount[]> {
  try {
    return await invoke<TagCount[]>("list_tags", { snippets: mode === "snippets" });
  } catch {
    return tagCounts;
  }
}

// ---------- 列表读取 ----------

function updateCount(query: string): void {
  const terms = query.split(/\s+/).filter(Boolean);
  const unit = mode === "snippets" ? "个片段" : "条记录";
  let text: string;
  if (terms.length > 1) text = `${entries.length} ${unit}同时包含 ${terms.length} 个关键词`;
  else if (query) text = `${entries.length} ${unit}匹配`;
  else if (activeTag) text = `${entries.length} ${unit}`;
  else text = mode === "snippets" ? `共 ${entries.length} 个片段` : `最近 ${entries.length} 条`;
  resultCount.textContent = activeTag ? `#${activeTag} · ${text}` : text;
}

function showEmptyState(query: string): void {
  const filtered = Boolean(query || activeTag);
  if (mode === "snippets") {
    if (filtered) setState("没有匹配的片段", "标题、正文和标签都会参与搜索；空格分隔的多个关键词需要同时命中", "empty");
    else setState("还没有代码片段", "点击右上角 + 新建，保存常用代码与文本", "empty");
  } else if (activeTag && !query) {
    setState(`没有 #${activeTag} 的记录`, "选中一条记录后按 Ctrl+T 为它打标签", "empty");
  } else if (filtered) {
    setState("没有找到", "空格分隔多个关键词表示同时包含；也可以搜标签或无声调全拼", "empty");
  } else {
    setState("还没有剪藏", "复制文字或图片后，记录会出现在这里", "empty");
  }
}

async function loadEntries(options: { loading?: boolean; focusAfter?: HTMLElement } = {}): Promise<void> {
  if (!hasNativeBackend) return;
  const query = searchInput.value.trim();
  const tag = activeTag;
  const version = ++requestVersion;
  const preserveSelection = !resetSelectionPending && query === displayedQuery && sameTag(tag, displayedTag);
  if (options.loading) {
    historyRegion.setAttribute("aria-busy", "true");
    setState("正在读取剪藏", "记录只在这台电脑上处理", "loading");
  } else {
    searchStatus.textContent = "搜索中…";
  }
  try {
    const listPromise = mode === "snippets"
      ? invoke<Snippet[]>("list_snippets", { query, tag }).then((snippets) => snippets.map((s): Entry => ({
        id: s.id, kind: "snippet", text: s.title, summary: s.title, created_at: s.updated_at,
        image_path: null, thumbnail_path: null, width: null, height: null,
        ocr_status: "none", ocr_error: null, tags: s.tags ?? [],
      })))
      : invoke<Entry[]>("list_entries", { query, tag });
    const [nextEntries, nextTags] = await Promise.all([listPromise, loadTags()]);
    if (version !== requestVersion) return;
    displayedQuery = query;
    displayedTag = tag;
    tagCounts = nextTags;
    renderTagBar();
    renderEntries(nextEntries.map((entry) => ({ ...entry, tags: entry.tags ?? [] })), preserveSelection);
    if (resetSelectionPending || !preserveSelection) historyList.scrollTop = 0;
    resetSelectionPending = false;
    updateCount(query);
    searchStatus.textContent = "";
    if (nextEntries.length === 0) showEmptyState(query);
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
  if (!hasNativeBackend || selectedId === null || pasteInFlight || dialogOpen() || searchInput.value.trim() !== displayedQuery) return;
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

// ---------- 设置表单 ----------

type FieldKind = "text" | "number" | "color" | "hotkey";
type FieldDef = {
  section: keyof Config;
  key: string;
  label: string;
  kind: FieldKind;
  hint?: string;
  min?: number;
  max?: number;
  wide?: boolean;
};
type FieldGroup = { title: string; description: string; fields: FieldDef[]; presets?: boolean };

const SETTINGS_GROUPS: FieldGroup[] = [
  {
    title: "快捷键",
    description: "聚焦输入框后直接按下组合键即可录制，也可以手动输入，例如 Ctrl+Alt+V。",
    fields: [
      { section: "hotkeys", key: "toggle", label: "剪贴板面板", kind: "hotkey" },
      { section: "hotkeys", key: "snippets", label: "代码片段面板", kind: "hotkey" },
      { section: "hotkeys", key: "paste", label: "粘贴按键", kind: "hotkey", hint: "发送给目标窗口", wide: true },
    ],
  },
  {
    title: "外观",
    description: "修改即时预览，保存后生效。颜色支持 #RRGGBB 或 rgba()。",
    presets: true,
    fields: [
      { section: "appearance", key: "accent_color", label: "强调色", kind: "color" },
      { section: "appearance", key: "text_color", label: "主文字", kind: "color" },
      { section: "appearance", key: "muted_color", label: "次要文字", kind: "color" },
      { section: "appearance", key: "background_color", label: "窗口背景", kind: "color" },
      { section: "appearance", key: "card_color", label: "卡片背景", kind: "color" },
      { section: "appearance", key: "border_color", label: "边框", kind: "color" },
      { section: "appearance", key: "selected_color", label: "选中背景", kind: "color" },
      { section: "appearance", key: "selected_border_color", label: "选中边框", kind: "color" },
      { section: "appearance", key: "font_family", label: "字体", kind: "text", wide: true },
      { section: "appearance", key: "font_size", label: "字号", kind: "number", min: 10, max: 32, hint: "10–32" },
      { section: "appearance", key: "width", label: "窗口宽度", kind: "number", min: 280, max: 1200, hint: "280–1200" },
      { section: "appearance", key: "height", label: "窗口高度", kind: "number", min: 320, max: 1200, hint: "320–1200" },
    ],
  },
  {
    title: "历史记录",
    description: "超出上限时自动删除最旧的记录；标签不影响清理顺序。",
    fields: [
      { section: "history", key: "display_limit", label: "每次显示", kind: "number", min: 1, max: 100, hint: "1–100 条" },
      { section: "history", key: "max_items", label: "最多保存", kind: "number", min: 1, max: 5000, hint: "1–5000 条" },
      { section: "history", key: "max_image_mb", label: "单张图片上限", kind: "number", min: 1, max: 128, hint: "MB" },
      { section: "history", key: "max_text_kb", label: "单条文字上限", kind: "number", min: 1, max: 1024, hint: "KB" },
      { section: "ocr", key: "language", label: "OCR 语言", kind: "text", hint: "如 zh-Hans、en-US" },
    ],
  },
];

type Preset = { name: string; colors: Partial<Appearance> };
const PRESETS: Preset[] = [
  {
    name: "暖纸",
    colors: {
      text_color: "#28332D", muted_color: "#6B756F", background_color: "rgba(247, 242, 228, 0.78)",
      selected_color: "rgba(221, 232, 218, 0.92)", selected_border_color: "#789080",
      card_color: "rgba(255, 253, 246, 0.76)", border_color: "rgba(61, 77, 68, 0.16)", accent_color: "#587461",
    },
  },
  {
    name: "雾蓝",
    colors: {
      text_color: "#1F2A37", muted_color: "#64748B", background_color: "rgba(241, 245, 250, 0.82)",
      selected_color: "rgba(214, 228, 247, 0.95)", selected_border_color: "#6C8EBF",
      card_color: "rgba(255, 255, 255, 0.72)", border_color: "rgba(30, 50, 80, 0.14)", accent_color: "#3F6FB5",
    },
  },
  {
    name: "暮紫",
    colors: {
      text_color: "#2E2638", muted_color: "#75697F", background_color: "rgba(246, 242, 250, 0.82)",
      selected_color: "rgba(230, 220, 244, 0.95)", selected_border_color: "#9A84BC",
      card_color: "rgba(255, 255, 255, 0.70)", border_color: "rgba(60, 40, 90, 0.14)", accent_color: "#7A5CA8",
    },
  },
  {
    name: "夜墨",
    colors: {
      text_color: "#E6EAE7", muted_color: "#9AA5A0", background_color: "rgba(28, 32, 30, 0.88)",
      selected_color: "rgba(88, 122, 100, 0.42)", selected_border_color: "#8FB39C",
      card_color: "rgba(255, 255, 255, 0.06)", border_color: "rgba(255, 255, 255, 0.11)", accent_color: "#8FC7A2",
    },
  },
];

const fieldInputs = new Map<string, { def: FieldDef; input: HTMLInputElement; swatch?: HTMLElement; picker?: HTMLInputElement }>();
const fieldId = (def: FieldDef): string => `${def.section}.${def.key}`;
type ConfigRecord = Record<string, Record<string, string | number>>;

const HEX_COLOR = /^#[0-9a-f]{6}$/i;

function updateSwatch(entry: { input: HTMLInputElement; swatch?: HTMLElement; picker?: HTMLInputElement }): void {
  if (!entry.swatch) return;
  entry.swatch.style.setProperty("--swatch", entry.input.value.trim() || "transparent");
  if (entry.picker && HEX_COLOR.test(entry.input.value.trim())) entry.picker.value = entry.input.value.trim().toLowerCase();
}

const KEY_NAMES: Record<string, string> = {
  Insert: "Insert", Delete: "Delete", Home: "Home", End: "End", PageUp: "PageUp", PageDown: "PageDown",
  Space: "Space", Enter: "Enter", Pause: "Pause", PrintScreen: "PrintScreen",
};

function chordFromEvent(event: KeyboardEvent): string | null {
  let key: string | undefined;
  if (/^Key[A-Z]$/.test(event.code)) key = event.code.slice(3);
  else if (/^Digit\d$/.test(event.code)) key = event.code.slice(5);
  else if (/^F\d{1,2}$/.test(event.code)) key = event.code;
  else key = KEY_NAMES[event.code];
  if (!key) return null;
  const special = /^F\d{1,2}$/.test(key) || key === "Insert" || key === "Pause" || key === "PrintScreen";
  if (!event.ctrlKey && !event.altKey && !event.metaKey && !special) return null;
  const parts: string[] = [];
  if (event.ctrlKey) parts.push("Ctrl");
  if (event.altKey) parts.push("Alt");
  if (event.shiftKey) parts.push("Shift");
  if (event.metaKey) parts.push("Win");
  parts.push(key);
  return parts.join("+");
}

function buildSettingsForm(): void {
  clearNode(settingsFields);
  for (const group of SETTINGS_GROUPS) {
    const section = document.createElement("section");
    section.className = "settings-group";
    const heading = document.createElement("h3");
    heading.textContent = group.title;
    const description = document.createElement("p");
    description.textContent = group.description;
    section.append(heading, description);
    if (group.presets) {
      const presets = document.createElement("div");
      presets.className = "preset-row";
      presets.setAttribute("aria-label", "配色方案");
      for (const preset of PRESETS) {
        const button = document.createElement("button");
        button.type = "button";
        button.className = "preset-button";
        button.style.setProperty("--preset-bg", preset.colors.background_color ?? "");
        button.style.setProperty("--preset-accent", preset.colors.accent_color ?? "");
        button.style.setProperty("--preset-text", preset.colors.text_color ?? "");
        const dot = document.createElement("span");
        dot.setAttribute("aria-hidden", "true");
        button.append(dot, document.createTextNode(preset.name));
        button.addEventListener("click", () => {
          for (const [key, value] of Object.entries(preset.colors)) {
            const field = fieldInputs.get(`appearance.${key}`);
            if (field && typeof value === "string") { field.input.value = value; updateSwatch(field); }
          }
          onSettingsInput(true);
        });
        presets.append(button);
      }
      section.append(presets);
    }
    const grid = document.createElement("div");
    grid.className = "field-grid";
    for (const def of group.fields) {
      const label = document.createElement("label");
      label.className = `field${def.wide ? " wide" : ""}`;
      const caption = document.createElement("span");
      caption.className = "field-label";
      caption.textContent = def.label;
      if (def.hint) {
        const hint = document.createElement("small");
        hint.textContent = def.hint;
        caption.append(hint);
      }
      const control = document.createElement("span");
      control.className = `field-control kind-${def.kind}`;
      const input = document.createElement("input");
      input.name = fieldId(def);
      input.autocomplete = "off";
      input.spellcheck = false;
      const entry: { def: FieldDef; input: HTMLInputElement; swatch?: HTMLElement; picker?: HTMLInputElement } = { def, input };
      if (def.kind === "number") {
        input.type = "number";
        input.inputMode = "numeric";
        input.step = "1";
        if (def.min !== undefined) input.min = String(def.min);
        if (def.max !== undefined) input.max = String(def.max);
      } else {
        input.type = "text";
        input.maxLength = def.kind === "text" ? 200 : 64;
      }
      if (def.kind === "color") {
        const swatch = document.createElement("span");
        swatch.className = "swatch";
        const picker = document.createElement("input");
        picker.type = "color";
        picker.tabIndex = -1;
        picker.setAttribute("aria-label", `${def.label}取色器`);
        picker.addEventListener("input", () => {
          input.value = picker.value.toUpperCase();
          updateSwatch(entry);
          onSettingsInput(true);
        });
        swatch.append(picker);
        control.append(swatch);
        entry.swatch = swatch;
        entry.picker = picker;
      }
      if (def.kind === "hotkey") {
        input.placeholder = "按下组合键";
        input.addEventListener("keydown", (event) => {
          if (event.isComposing) return;
          const chord = chordFromEvent(event);
          if (!chord) return;
          event.preventDefault();
          event.stopPropagation();
          input.value = chord;
          onSettingsInput(false);
        });
      }
      input.addEventListener("input", () => {
        updateSwatch(entry);
        onSettingsInput(def.section === "appearance");
      });
      control.append(input);
      label.append(caption, control);
      grid.append(label);
      fieldInputs.set(fieldId(def), entry);
    }
    section.append(grid);
    settingsFields.append(section);
  }
}

function fillSettingsForm(config: Config): void {
  const record = config as unknown as ConfigRecord;
  for (const entry of fieldInputs.values()) {
    const value = record[entry.def.section]?.[entry.def.key];
    entry.input.value = value === undefined ? "" : String(value);
    entry.input.removeAttribute("aria-invalid");
    updateSwatch(entry);
  }
  setSettingsStatus("修改后保存，立即生效并写回 config.yaml");
  updateSettingsDirty();
}

function readSettingsForm(): { config: Config | null; invalid: HTMLInputElement[] } {
  if (!currentConfig) return { config: null, invalid: [] };
  const draft = structuredClone(currentConfig) as unknown as ConfigRecord;
  const invalid: HTMLInputElement[] = [];
  for (const { def, input } of fieldInputs.values()) {
    const raw = input.value.trim();
    let ok = raw.length > 0;
    let value: string | number = raw;
    if (def.kind === "number") {
      const number = Number(raw);
      ok = ok && Number.isInteger(number) && (def.min === undefined || number >= def.min) && (def.max === undefined || number <= def.max);
      value = number;
    } else if (def.kind === "color" || def.section === "appearance") {
      ok = ok && !/[;{}]/.test(raw);
    }
    input.toggleAttribute("aria-invalid", !ok);
    if (!ok) invalid.push(input);
    const section = draft[def.section];
    if (section && ok) section[def.key] = value;
  }
  return { config: draft as unknown as Config, invalid };
}

function isSettingsDirty(): boolean {
  if (!currentConfig) return false;
  const { config, invalid } = readSettingsForm();
  return invalid.length > 0 || JSON.stringify(config) !== JSON.stringify(currentConfig);
}

function setSettingsStatus(text: string, tone: "idle" | "dirty" | "error" | "ok" = "idle"): void {
  settingsStatus.textContent = text;
  settingsStatus.dataset.tone = tone;
}

function updateSettingsDirty(): boolean {
  const dirty = isSettingsDirty();
  settingsRevert.disabled = !dirty || savingSettings;
  settingsSave.disabled = !dirty || savingSettings || !hasNativeBackend;
  return dirty;
}

function onSettingsInput(previewAppearance: boolean): void {
  const dirty = updateSettingsDirty();
  if (dirty) setSettingsStatus("有未保存的修改", "dirty");
  else setSettingsStatus("修改后保存，立即生效并写回 config.yaml");
  if (previewAppearance && currentConfig) {
    const { config } = readSettingsForm();
    if (config) applyAppearance(config.appearance);
  }
}

async function saveSettings(): Promise<void> {
  if (savingSettings || !hasNativeBackend || !currentConfig) return;
  const { config, invalid } = readSettingsForm();
  if (!config || invalid.length > 0) {
    setSettingsStatus("请检查标红的字段", "error");
    invalid[0]?.focus();
    return;
  }
  savingSettings = true;
  updateSettingsDirty();
  setSettingsStatus("正在保存…");
  try {
    const saved = await invoke<Config>("save_settings", { config });
    applySettings(saved);
    fillSettingsForm(saved);
    setSettingsStatus("已保存，并写回配置文件", "ok");
  } catch (error) {
    setSettingsStatus(errorText(error), "error");
  } finally {
    savingSettings = false;
    updateSettingsDirty();
  }
}

function openSettings(): void {
  if (dialogOpen()) return;
  panelOpen = true;
  syncPreview(null, "");
  if (currentConfig) fillSettingsForm(currentConfig);
  settingsPanel.hidden = false;
  settingsScrim.hidden = false;
  settingsToggle.setAttribute("aria-expanded", "true");
  requestAnimationFrame(() => {
    root.classList.add("settings-open");
    settingsClose.focus();
  });
}

function closeSettings(restoreFocus = true, force = false): void {
  if (!panelOpen) return;
  if (!force && isSettingsDirty() && !window.confirm("放弃未保存的设置修改？")) return;
  panelOpen = false;
  if (currentConfig) {
    applyAppearance(currentConfig.appearance);
    fillSettingsForm(currentConfig);
  }
  root.classList.remove("settings-open");
  settingsToggle.setAttribute("aria-expanded", "false");
  settingsPanel.hidden = true;
  settingsScrim.hidden = true;
  if (restoreFocus) searchInput.focus();
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
      fillSettingsForm(config);
      setSettingsStatus("已从配置文件重新载入", "ok");
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

function resetForPopup(nextMode: Mode = "clipboard"): void {
  if (snippetEditor.open) { snippetTitle.focus(); return; }
  if (tagEditor.open) closeTagEditor(false);
  const modeChanged = nextMode !== mode;
  mode = nextMode;
  snippetTools.hidden = mode !== "snippets";
  root.dataset.mode = mode;
  updateModeLabels();
  searchInput.placeholder = mode === "snippets" ? "搜索标题、内容或标签 · 空格分隔多个关键词" : "搜索剪贴板 · 空格分隔多个关键词";
  historyList.setAttribute("aria-label", mode === "snippets" ? "代码片段" : "剪贴板记录");
  searchInput.setAttribute("aria-label", searchInput.placeholder);
  requireElement("history-settings").hidden = mode === "snippets";
  requireElement("ocr-settings").hidden = mode === "snippets";
  historyRefreshPending = false;
  window.clearTimeout(searchTimer);
  searchInput.value = "";
  displayedQuery = "";
  activeTag = null;
  displayedTag = null;
  if (modeChanged) tagCounts = [];
  renderTagBar();
  selectedId = null;
  resetSelectionPending = true;
  historyList.scrollTop = 0;
  renderSelection();
  clearError();
  if (panelOpen) closeSettings(false, true);
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

// ---------- 标签编辑 ----------

function renderTagSuggestions(): void {
  const current = new Set(parseTags(tagInput.value).map((tag) => tag.toLowerCase()));
  clearNode(tagSuggestions);
  const names = tagCounts.map((tag) => tag.name).slice(0, 24);
  tagSuggestions.hidden = names.length === 0;
  for (const name of names) {
    const chip = document.createElement("button");
    chip.type = "button";
    chip.className = "filter-chip small";
    chip.textContent = `#${name}`;
    chip.setAttribute("aria-pressed", String(current.has(name.toLowerCase())));
    chip.addEventListener("click", () => {
      const tags = parseTags(tagInput.value);
      const index = tags.findIndex((tag) => sameTag(tag, name));
      if (index >= 0) tags.splice(index, 1);
      else tags.push(name);
      tagInput.value = tags.join(" ");
      renderTagSuggestions();
      tagInput.focus();
    });
    tagSuggestions.append(chip);
  }
}

function openTagEditor(): void {
  if (!hasNativeBackend || selectedId === null || dialogOpen()) return;
  const entry = entries.find((item) => item.id === selectedId);
  if (!entry) return;
  if (panelOpen) closeSettings(false);
  if (panelOpen) return;
  tagEditingId = entry.id;
  tagInput.value = entry.tags.join(" ");
  tagEditorTarget.textContent = `${kindLabel(entry.kind)} · ${entryTitle(entry).replace(/\s+/g, " ").slice(0, 120)}`;
  tagError.textContent = "";
  renderTagSuggestions();
  tagEditor.showModal();
  renderSelection();
  tagInput.focus();
  tagInput.setSelectionRange(tagInput.value.length, tagInput.value.length);
}

function closeTagEditor(restoreFocus = true): void {
  if (tagEditor.open) tagEditor.close();
  tagEditingId = null;
  if (restoreFocus) searchInput.focus();
  renderSelection();
}

async function saveTags(): Promise<void> {
  if (savingTags || !tagEditor.open || tagEditingId === null) return;
  const tags = parseTags(tagInput.value);
  if (tags.length > MAX_TAGS) {
    tagError.textContent = `最多 ${MAX_TAGS} 个标签`;
    return;
  }
  const id = tagEditingId;
  savingTags = true;
  tagSave.disabled = true;
  try {
    if (mode === "snippets") {
      const snippet = await invoke<Snippet>("get_snippet", { id });
      await invoke("save_snippet", { id, title: snippet.title, content: snippet.content, tags });
    } else {
      await invoke("set_entry_tags", { id, tags });
    }
    closeTagEditor();
    await loadEntries();
  } catch (error) {
    tagError.textContent = errorText(error);
  } finally {
    savingTags = false;
    tagSave.disabled = false;
  }
}

// ---------- 键盘 ----------

function trapFocus(event: KeyboardEvent, container: HTMLElement): void {
  const focusable = Array.from(container.querySelectorAll<HTMLElement>(
    "button:not(:disabled):not([tabindex='-1']), [href], input:not(:disabled):not([tabindex='-1']), textarea:not(:disabled), [tabindex]:not([tabindex='-1'])",
  )).filter((element) => element.offsetParent !== null);
  const first = focusable[0];
  const last = focusable[focusable.length - 1];
  if (!first || !last) return;
  if (event.shiftKey && document.activeElement === first) {
    event.preventDefault();
    last.focus();
  } else if (!event.shiftKey && document.activeElement === last) {
    event.preventDefault();
    first.focus();
  }
}

function handleKeyboard(event: KeyboardEvent): void {
  if (event.isComposing || composing || event.key === "Process") return;
  if (snippetEditor.open) {
    if (event.key === "Enter" && event.ctrlKey) { event.preventDefault(); void saveSnippet(); }
    return;
  }
  if (tagEditor.open) {
    if (event.key === "Enter" && !(event.target instanceof HTMLElement && event.target.closest("button"))) {
      event.preventDefault();
      void saveTags();
    }
    return;
  }
  if (panelOpen) {
    if (event.key === "Escape") {
      event.preventDefault();
      closeSettings();
    } else if (event.key === "Tab") {
      trapFocus(event, settingsPanel);
    }
    return;
  }
  if (event.key === "Enter" && event.target instanceof HTMLElement && event.target.closest("button")) return;
  if (event.ctrlKey && !event.altKey && event.key.toLowerCase() === "t") {
    event.preventDefault();
    openTagEditor();
    return;
  }
  if (event.altKey && (event.key === "ArrowLeft" || event.key === "ArrowRight")) {
    event.preventDefault();
    cycleTagFilter(event.key === "ArrowRight" ? 1 : -1);
    return;
  }
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
      if (activeTag && !searchInput.value) setTagFilter(null);
      else void hidePopup();
      break;
  }
}

async function initializeNative(): Promise<void> {
  try {
    const config = await invoke<Config>("get_settings");
    applySettings(config);
    fillSettingsForm(config);
    await Promise.all([
      listen<string>("popup-shown", (event) => resetForPopup(event.payload === "snippets" ? "snippets" : "clipboard")),
      listen("history-changed", () => { if (mode === "clipboard") refreshForHistoryChange(); }),
      listen("snippets-changed", () => { if (mode === "snippets") refreshForHistoryChange(); }),
      listen<string>("app-error", (event) => showError(event.payload)),
      listen<Config>("settings-changed", (event) => {
        const dirty = panelOpen && isSettingsDirty();
        applySettings(event.payload);
        if (!dirty) fillSettingsForm(event.payload);
      }),
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
  settingsSave.disabled = true;
  settingsRevert.disabled = true;
  setSettingsStatus("设置需要在桌面应用中修改");
}

buildSettingsForm();

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
settingsForm.addEventListener("submit", (event) => {
  event.preventDefault();
  void saveSettings();
});
settingsRevert.addEventListener("click", () => {
  if (!currentConfig) return;
  applyAppearance(currentConfig.appearance);
  fillSettingsForm(currentConfig);
});
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
tagEditButton.addEventListener("click", openTagEditor);
tagInput.addEventListener("input", renderTagSuggestions);
tagSave.addEventListener("click", () => void saveTags());
requireElement("tag-cancel").addEventListener("click", () => closeTagEditor());
tagEditor.addEventListener("cancel", (event) => { event.preventDefault(); closeTagEditor(); });

// ---------- 代码片段编辑 ----------

const snippetDraft = (): string => JSON.stringify([snippetTitle.value, snippetTags.value, snippetContent.value]);

async function openSnippetEditor(id: number | null): Promise<void> {
  if (!hasNativeBackend || dialogOpen()) return;
  try {
    const snippet = id === null ? null : await invoke<Snippet>("get_snippet", { id });
    editingId = id;
    snippetTitle.value = snippet?.title ?? "";
    snippetTags.value = (snippet?.tags ?? (activeTag ? [activeTag] : [])).join(" ");
    snippetContent.value = snippet?.content ?? "";
    originalDraft = snippetDraft();
    requireElement("editor-title").textContent = id === null ? "新增代码片段" : "编辑代码片段";
    requireElement("snippet-error").textContent = "";
    if (panelOpen) closeSettings(false, true);
    snippetEditor.showModal();
    renderSelection();
    snippetTitle.focus();
  } catch (error) { showError(error); }
}

function cancelSnippet(): void {
  if (savingSnippet) return;
  if (snippetDraft() !== originalDraft && !window.confirm("放弃未保存的片段修改？")) return;
  snippetEditor.close();
  searchInput.focus();
  renderSelection();
}

async function saveSnippet(): Promise<void> {
  if (savingSnippet || !snippetEditor.open) return;
  if (!snippetTitle.value.trim() || !snippetContent.value) {
    requireElement("snippet-error").textContent = "请填写标题和内容"; return;
  }
  const tags = parseTags(snippetTags.value);
  if (tags.length > MAX_TAGS) {
    requireElement("snippet-error").textContent = `最多 ${MAX_TAGS} 个标签`; return;
  }
  savingSnippet = true; snippetSave.disabled = true;
  try {
    await invoke("save_snippet", { id: editingId, title: snippetTitle.value, content: snippetContent.value, tags });
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
