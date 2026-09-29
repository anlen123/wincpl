import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./style.css";
import "./preview.css";

type Preview = {
  id: number;
  kind: string;
  text: string;
  summary: string;
  image_path: string | null;
  width: number | null;
  height: number | null;
  created_at: number;
  tags?: string[];
};
type PreviewPayload = { entry: Preview | null; terms: string[] };
const title = document.getElementById("preview-title")!;
const eyebrow = document.getElementById("preview-eyebrow")!;
const tagsRow = document.getElementById("preview-tags")!;
const content = document.getElementById("preview-content")!;
const text = document.getElementById("preview-text")!;
const image = document.getElementById("preview-image") as HTMLImageElement;
const status = document.getElementById("preview-status")!;
const meta = document.getElementById("preview-meta")!;
const scale = document.getElementById("image-scale") as HTMLButtonElement;
let version = 0;

function countLines(value: string): number {
  return value ? value.split(/\r\n|\r|\n/).length : 0;
}

/** 把文本按关键词切分并生成高亮 span，避免使用 innerHTML 带来的注入风险。 */
function renderHighlighted(target: HTMLElement, value: string, terms: string[]): void {
  const needles = terms.filter(Boolean);
  if (needles.length === 0 || !value) {
    target.textContent = value;
    return;
  }
  const lower = value.toLowerCase();
  const nodes: Node[] = [];
  let cursor = 0;
  while (cursor < value.length) {
    let bestStart = -1;
    let bestLength = 0;
    for (const needle of needles) {
      const at = lower.indexOf(needle, cursor);
      if (at < 0) continue;
      if (bestStart < 0 || at < bestStart || (at === bestStart && needle.length > bestLength)) {
        bestStart = at;
        bestLength = needle.length;
      }
    }
    if (bestStart < 0) {
      nodes.push(document.createTextNode(value.slice(cursor)));
      break;
    }
    if (bestStart > cursor) nodes.push(document.createTextNode(value.slice(cursor, bestStart)));
    const mark = document.createElement("mark");
    mark.className = "preview-hit";
    mark.textContent = value.slice(bestStart, bestStart + bestLength);
    nodes.push(mark);
    cursor = bestStart + bestLength;
  }
  target.replaceChildren(...nodes);
}

function renderTags(tags: string[]): void {
  tagsRow.replaceChildren(...tags.map((tag) => {
    const chip = document.createElement("span");
    chip.textContent = `#${tag}`;
    return chip;
  }));
  tagsRow.hidden = tags.length === 0;
}

function render(payload: PreviewPayload): void {
  const entry = payload.entry;
  const terms = payload.terms ?? [];
  image.removeAttribute("src");
  image.hidden = text.hidden = scale.hidden = true;
  text.textContent = "";
  document.body.dataset.kind = entry?.kind ?? "none";
  content.classList.remove("actual-size");
  scale.textContent = "原始尺寸";
  content.scrollTop = content.scrollLeft = 0;
  status.hidden = entry !== null;
  status.textContent = "选择一条记录查看完整内容";
  meta.textContent = "";
  renderTags(entry?.tags ?? []);
  if (!entry) {
    eyebrow.textContent = "剪藏 / PREVIEW";
    title.textContent = "完整预览";
    return;
  }
  if (entry.kind === "image" && entry.image_path) {
    eyebrow.textContent = "剪藏 / 图片";
    renderHighlighted(title, "图片预览", terms);
    meta.textContent = `${entry.width} × ${entry.height} · 原图`;
    image.src = convertFileSrc(entry.image_path);
    image.hidden = scale.hidden = false;
  } else if (entry.kind === "snippet") {
    eyebrow.textContent = "剪藏 / 代码片段";
    renderHighlighted(title, entry.summary || "代码片段", terms);
    renderHighlighted(text, entry.text, terms);
    text.hidden = false;
    meta.textContent = `${countLines(entry.text)} 行 · ${entry.text.length} 字符 · Enter 粘贴`;
  } else {
    eyebrow.textContent = "剪藏 / 文字";
    renderHighlighted(title, "文字预览", terms);
    renderHighlighted(text, entry.text, terms);
    text.hidden = false;
    meta.textContent = `${countLines(entry.text)} 行 · ${entry.text.length} 字符`;
  }
}
async function refresh(): Promise<void> {
  const current = ++version;
  try {
    const payload = await invoke<PreviewPayload>("get_preview");
    if (current === version) render(payload ?? { entry: null, terms: [] });
  } catch (error) {
    if (current !== version) return;
    render({ entry: null, terms: [] });
    status.textContent = String(error);
  }
}
function applyAppearance(config: { appearance: Record<string, string | number> }): void {
  const style = document.documentElement.style;
  for (const key of [
    "text_color", "muted_color", "background_color", "card_color", "border_color",
    "accent_color", "selected_color", "selected_border_color",
  ]) {
    style.setProperty(`--${key.replaceAll("_", "-")}`, String(config.appearance[key]));
  }
  style.setProperty("--ui-font", String(config.appearance.font_family));
  style.setProperty("--font-size", `${config.appearance.font_size}px`);
}
scale.addEventListener("click", () => {
  const actual = content.classList.toggle("actual-size");
  scale.textContent = actual ? "适应窗口" : "原始尺寸";
});
image.addEventListener("error", () => { status.hidden = false; status.textContent = "原图无法读取，可能已被删除"; });
document.addEventListener("keydown", (event) => {
  if (event.key === "Escape") { event.preventDefault(); void invoke("hide_popup"); }
});
async function initialize(): Promise<void> {
  await listen("preview-changed", () => void refresh());
  await listen<{ appearance: Record<string, string | number> }>("settings-changed", event => applyAppearance(event.payload));
  applyAppearance(await invoke("get_settings"));
  await refresh();
}
if ("__TAURI_INTERNALS__" in window) void initialize().catch(error => { status.textContent = String(error); });
