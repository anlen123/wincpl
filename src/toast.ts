import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./style.css";
import "./toast.css";

type Appearance = {
  text_color?: string;
  muted_color?: string;
  background_color?: string;
  selected_color?: string;
  selected_border_color?: string;
  card_color?: string;
  border_color?: string;
  accent_color?: string;
  font_family?: string;
  font_size?: number;
};

type ToastPayload = {
  kind: string;
  preview: string;
  detail: string;
  duration_ms: number;
};

const MIN_DURATION_MS = 600;
const MAX_DURATION_MS = 8000;
const DEFAULT_DURATION_MS = 2000;

const card = document.getElementById("toast-card") as HTMLDivElement;
const badge = document.getElementById("toast-badge") as HTMLSpanElement;
const titleText = document.getElementById("toast-title") as HTMLSpanElement;
const previewText = document.getElementById("toast-preview") as HTMLParagraphElement;
const metaText = document.getElementById("toast-meta") as HTMLSpanElement;

let leaveTimer: number | undefined;

/** 把 #RRGGBB / rgb() / rgba() 摊平到不透明白底上，保证浮窗压在任意桌面内容上都清晰可读。 */
function flattenColor(value: string): string | null {
  const input = value.trim();
  if (!input) return null;
  let r: number;
  let g: number;
  let b: number;
  let alpha = 1;

  const hex = /^#([0-9a-f]{3,8})$/i.exec(input);
  if (hex) {
    const digits = hex[1]!;
    const expand = (part: string) => Number.parseInt(part.length === 1 ? part + part : part, 16);
    if (digits.length === 3 || digits.length === 4) {
      r = expand(digits[0]!);
      g = expand(digits[1]!);
      b = expand(digits[2]!);
      if (digits.length === 4) alpha = expand(digits[3]!) / 255;
    } else if (digits.length === 6 || digits.length === 8) {
      r = expand(digits.slice(0, 2));
      g = expand(digits.slice(2, 4));
      b = expand(digits.slice(4, 6));
      if (digits.length === 8) alpha = expand(digits.slice(6, 8)) / 255;
    } else {
      return null;
    }
    if (![r, g, b, alpha].every(Number.isFinite)) return null;
    r = Math.min(255, Math.max(0, r));
    g = Math.min(255, Math.max(0, g));
    b = Math.min(255, Math.max(0, b));
    alpha = Math.min(1, Math.max(0, alpha));
  } else {
    const fn = /^rgba?\(([^)]*)\)$/i.exec(input);
    if (!fn) return null;
    const parts = fn[1]!.split(/[\s,/]+/).filter((part) => part.length > 0);
    if (parts.length < 3 || parts.length > 4) return null;
    const channel = (part: string) =>
      part.endsWith("%") ? (Number.parseFloat(part) / 100) * 255 : Number.parseFloat(part);
    r = channel(parts[0]!);
    g = channel(parts[1]!);
    b = channel(parts[2]!);
    alpha = parts[3] === undefined ? 1 : Number.parseFloat(parts[3]!);
    if (![r, g, b, alpha].every(Number.isFinite)) return null;
    r = Math.min(255, Math.max(0, Math.round(r)));
    g = Math.min(255, Math.max(0, Math.round(g)));
    b = Math.min(255, Math.max(0, Math.round(b)));
    alpha = Math.min(1, Math.max(0, alpha));
  }

  const blend = (channel: number) => Math.round(channel * alpha + 255 * (1 - alpha));
  return `rgb(${blend(r)}, ${blend(g)}, ${blend(b)})`;
}

function applyAppearance(appearance: Appearance | null | undefined): void {
  if (!appearance) return;
  const style = document.documentElement.style;
  for (const key of ["text_color", "muted_color", "border_color", "accent_color"] as const) {
    const value = appearance[key];
    if (typeof value === "string" && value.trim()) {
      style.setProperty(`--${key.replace(/_/g, "-")}`, value.trim());
    }
  }
  // 卡片底色摊平为不透明色，浮窗才不会被桌面内容干扰。
  const surface = flattenColor(appearance.card_color ?? "") ?? flattenColor(appearance.background_color ?? "");
  if (surface) style.setProperty("--card-color", surface);
  if (appearance.font_family?.trim()) style.setProperty("--ui-font", appearance.font_family.trim());
  if (typeof appearance.font_size === "number" && appearance.font_size > 0) {
    style.setProperty("--font-size", `${appearance.font_size}px`);
  }
}

function clampDuration(value: number): number {
  if (!Number.isFinite(value)) return DEFAULT_DURATION_MS;
  return Math.min(MAX_DURATION_MS, Math.max(MIN_DURATION_MS, Math.round(value)));
}

const TOAST_KINDS: Record<string, { badge: string; title: string }> = {
  image: { badge: "图", title: "已复制图片" },
  code: { badge: "码", title: "收到短信验证码" },
};
const TOAST_FALLBACK = { badge: "文", title: "已复制到剪贴板" };

function render(payload: ToastPayload): void {
  const image = payload.kind === "image";
  const kind = TOAST_KINDS[payload.kind] ?? TOAST_FALLBACK;
  card.dataset.kind = payload.kind;
  badge.textContent = kind.badge;
  titleText.textContent = kind.title;
  previewText.textContent = image ? "已存入历史，之后能用图里的文字搜索" : payload.preview;
  metaText.textContent = payload.detail;

  const duration = clampDuration(payload.duration_ms);
  card.style.setProperty("--toast-duration", `${duration}ms`);
  // 去掉再重新加类并强制重排，让果冻动画每次复制都从头播放。
  card.classList.remove("is-in", "is-out", "is-running");
  void card.offsetWidth;
  card.classList.add("is-in", "is-running");

  window.clearTimeout(leaveTimer);
  leaveTimer = window.setTimeout(() => {
    card.classList.add("is-out");
  }, Math.max(0, duration - 300));
}

async function boot(): Promise<void> {
  try {
    const config = await invoke<{ appearance?: Appearance }>("get_settings");
    applyAppearance(config.appearance);
  } catch {
    // 读取失败时沿用 CSS 里的默认配色。
  }

  await listen<{ appearance?: Appearance } | null>("settings-changed", (event) => {
    applyAppearance(event.payload?.appearance);
  });

  await listen<ToastPayload>("toast-changed", (event) => {
    if (event.payload) render(event.payload);
  });
}

void boot();
