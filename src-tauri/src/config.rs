use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::{ErrorKind, Write},
    path::Path,
};

const DEFAULT_CONFIG: &str = include_str!("../../config.example.yaml");
const MAX_ITEMS: usize = 5_000;
const MAX_IMAGE_MB: u32 = 128;
const MAX_TEXT_KB: u32 = 1_024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub hotkeys: Hotkeys,
    pub appearance: Appearance,
    pub history: History,
    pub ocr: Ocr,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Hotkeys {
    pub toggle: String,
    #[serde(default = "default_snippet_shortcut")]
    pub snippets: String,
    pub paste: String,
}

fn default_snippet_shortcut() -> String {
    "Ctrl+Alt+S".into()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Appearance {
    pub text_color: String,
    pub muted_color: String,
    pub background_color: String,
    pub selected_color: String,
    pub selected_border_color: String,
    pub card_color: String,
    pub border_color: String,
    pub accent_color: String,
    pub font_family: String,
    pub font_size: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct History {
    #[serde(default = "default_display_limit")]
    pub display_limit: usize,
    pub max_items: usize,
    pub max_image_mb: u32,
    pub max_text_kb: u32,
}

fn default_display_limit() -> usize {
    20
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Ocr {
    pub language: String,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            if let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                fs::create_dir_all(parent)
                    .map_err(|error| format!("无法创建配置目录 {}：{error}", parent.display()))?;
            }
            match OpenOptions::new().write(true).create_new(true).open(path) {
                Ok(mut file) => {
                    if let Err(error) = file
                        .write_all(DEFAULT_CONFIG.as_bytes())
                        .and_then(|_| file.sync_all())
                    {
                        drop(file);
                        let _ = fs::remove_file(path);
                        return Err(format!("无法创建默认配置 {}：{error}", path.display()));
                    }
                }
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(format!("无法创建默认配置 {}：{error}", path.display()));
                }
            }
        }

        let source = fs::read_to_string(path)
            .map_err(|error| format!("无法读取配置 {}：{error}", path.display()))?;
        let config: Self = serde_yaml::from_str(source.trim_start_matches('\u{feff}'))
            .map_err(|error| format!("配置格式错误 {}：{error}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), String> {
        validate_shortcut_shape("呼出快捷键", &self.hotkeys.toggle)?;
        validate_shortcut_shape("粘贴按键", &self.hotkeys.paste)?;
        validate_shortcut_shape("片段快捷键", &self.hotkeys.snippets)?;
        if !(1..=100).contains(&self.history.display_limit) {
            return Err("history.display_limit 必须在 1 到 100 之间".into());
        }

        if !(1..=MAX_ITEMS).contains(&self.history.max_items) {
            return Err(format!("history.max_items 必须在 1 到 {MAX_ITEMS} 之间"));
        }
        if !(1..=MAX_IMAGE_MB).contains(&self.history.max_image_mb) {
            return Err(format!(
                "history.max_image_mb 必须在 1 到 {MAX_IMAGE_MB} 之间"
            ));
        }
        if !(1..=MAX_TEXT_KB).contains(&self.history.max_text_kb) {
            return Err(format!(
                "history.max_text_kb 必须在 1 到 {MAX_TEXT_KB} 之间"
            ));
        }
        if !(10..=32).contains(&self.appearance.font_size) {
            return Err("appearance.font_size 必须在 10 到 32 之间".into());
        }
        if !(280..=1_200).contains(&self.appearance.width) {
            return Err("appearance.width 必须在 280 到 1200 之间".into());
        }
        if !(320..=1_200).contains(&self.appearance.height) {
            return Err("appearance.height 必须在 320 到 1200 之间".into());
        }

        for (name, value) in [
            ("text_color", &self.appearance.text_color),
            ("muted_color", &self.appearance.muted_color),
            ("background_color", &self.appearance.background_color),
            ("selected_color", &self.appearance.selected_color),
            (
                "selected_border_color",
                &self.appearance.selected_border_color,
            ),
            ("card_color", &self.appearance.card_color),
            ("border_color", &self.appearance.border_color),
            ("accent_color", &self.appearance.accent_color),
        ] {
            validate_css_value(&format!("appearance.{name}"), value, 128)?;
        }
        validate_css_value("appearance.font_family", &self.appearance.font_family, 256)?;

        let language = self.ocr.language.as_str();
        if language.is_empty()
            || language.len() > 35
            || language.starts_with('-')
            || language.ends_with('-')
            || language.contains("--")
            || !language
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err("ocr.language 必须是有效的 Windows 语言标签，例如 zh-Hans 或 en-US".into());
        }
        Ok(())
    }
}

impl Config {
    /// 校验后把配置写回 YAML 文件；尽量保留原文件中的注释与排版。
    pub fn save(&self, path: &Path) -> Result<(), String> {
        self.validate()?;
        let source = fs::read_to_string(path)
            .map(|text| text.trim_start_matches('\u{feff}').to_string())
            .unwrap_or_default();
        let rendered = render_yaml(&source, self)
            .or_else(|| render_yaml(DEFAULT_CONFIG, self))
            .ok_or_else(|| "无法生成配置文件".to_string())?;
        let reparsed: Self = serde_yaml::from_str(&rendered)
            .map_err(|error| format!("生成的配置无法解析：{error}"))?;
        if &reparsed != self {
            return Err("生成的配置与设置不一致，已放弃写入".into());
        }
        let temporary = path.with_extension("yaml.tmp");
        fs::write(&temporary, rendered.as_bytes())
            .map_err(|error| format!("无法写入配置 {}：{error}", temporary.display()))?;
        fs::rename(&temporary, path).map_err(|error| {
            let _ = fs::remove_file(&temporary);
            format!("无法保存配置 {}：{error}", path.display())
        })
    }

    fn yaml_fields(&self) -> Vec<(&'static str, &'static str, String)> {
        let quote = |value: &str| yaml_quote(value);
        let a = &self.appearance;
        vec![
            ("hotkeys", "toggle", quote(&self.hotkeys.toggle)),
            ("hotkeys", "snippets", quote(&self.hotkeys.snippets)),
            ("hotkeys", "paste", quote(&self.hotkeys.paste)),
            ("appearance", "text_color", quote(&a.text_color)),
            ("appearance", "muted_color", quote(&a.muted_color)),
            ("appearance", "background_color", quote(&a.background_color)),
            ("appearance", "selected_color", quote(&a.selected_color)),
            ("appearance", "selected_border_color", quote(&a.selected_border_color)),
            ("appearance", "card_color", quote(&a.card_color)),
            ("appearance", "border_color", quote(&a.border_color)),
            ("appearance", "accent_color", quote(&a.accent_color)),
            ("appearance", "font_family", quote(&a.font_family)),
            ("appearance", "font_size", a.font_size.to_string()),
            ("appearance", "width", a.width.to_string()),
            ("appearance", "height", a.height.to_string()),
            ("history", "display_limit", self.history.display_limit.to_string()),
            ("history", "max_items", self.history.max_items.to_string()),
            ("history", "max_image_mb", self.history.max_image_mb.to_string()),
            ("history", "max_text_kb", self.history.max_text_kb.to_string()),
            ("ocr", "language", quote(&self.ocr.language)),
        ]
    }
}

fn yaml_quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for character in value.chars() {
        match character {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            _ => quoted.push(character),
        }
    }
    quoted.push('"');
    quoted
}

/// 返回值之后的注释部分（含前导空白），没有注释则为空。
fn trailing_comment(value: &str) -> &str {
    let bytes = value.as_bytes();
    let mut index = 0;
    if let Some(quote @ (b'"' | b'\'')) = bytes.first().copied() {
        index = 1;
        while index < bytes.len() {
            if bytes[index] == b'\\' && quote == b'"' {
                index += 2;
                continue;
            }
            if bytes[index] == quote {
                index += 1;
                break;
            }
            index += 1;
        }
    }
    let rest = &value[index.min(value.len())..];
    match rest.find(" #") {
        Some(position) => {
            let start = rest[..position].trim_end().len();
            &rest[start..]
        }
        None => "",
    }
}

/// 在模板上逐行替换 `section.key` 的值，保留注释；缺少字段时返回 None。
fn render_yaml(template: &str, config: &Config) -> Option<String> {
    let fields = config.yaml_fields();
    let mut written = vec![false; fields.len()];
    let mut section = "";
    let mut output = String::with_capacity(template.len() + 64);
    for line in template.lines() {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        let mut replaced = None;
        if !trimmed.is_empty() && !trimmed.starts_with('#') {
            if indent == 0 {
                section = trimmed.split(':').next().unwrap_or("").trim();
            } else if let Some((key, value)) = trimmed.split_once(':') {
                let key = key.trim();
                if let Some(index) = fields
                    .iter()
                    .position(|(s, k, _)| *s == section && *k == key)
                {
                    let comment = trailing_comment(value.trim_start());
                    replaced = Some(format!(
                        "{}{key}: {}{comment}",
                        &line[..indent],
                        fields[index].2
                    ));
                    written[index] = true;
                }
            }
        }
        output.push_str(replaced.as_deref().unwrap_or(line));
        output.push('\n');
    }
    written.iter().all(|done| *done).then_some(output)
}

fn validate_css_value(name: &str, value: &str, max_len: usize) -> Result<(), String> {
    if value.trim() != value
        || value.is_empty()
        || value.len() > max_len
        || value.chars().any(char::is_control)
        || value.contains(';')
        || value.contains('{')
        || value.contains('}')
    {
        return Err(format!("{name} 不是安全的 CSS 值"));
    }
    Ok(())
}

fn validate_shortcut_shape(name: &str, value: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > 64 || value.trim() != value {
        return Err(format!("{name} 不能为空、过长或包含首尾空格"));
    }

    let parts: Vec<&str> = value.split('+').collect();
    if parts.is_empty()
        || parts.len() > 5
        || parts.iter().any(|part| {
            part.is_empty()
                || part.trim() != *part
                || !part.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
    {
        return Err(format!("{name} 格式错误，请使用 Ctrl+Alt+V 形式"));
    }

    let mut modifiers = HashSet::new();
    for part in &parts[..parts.len() - 1] {
        let lower = part.to_ascii_lowercase();
        let modifier = match lower.as_str() {
            "ctrl" | "control" => "ctrl",
            "alt" => "alt",
            "shift" => "shift",
            "win" | "meta" | "cmd" | "command" | "super" => "system",
            _ => return Err(format!("{name} 的修饰键格式错误")),
        };
        if !modifiers.insert(modifier) {
            return Err(format!("{name} 含有重复的修饰键"));
        }
    }

    let key = parts[parts.len() - 1].to_ascii_lowercase();
    if matches!(
        key.as_str(),
        "ctrl" | "control" | "alt" | "shift" | "win" | "meta" | "cmd" | "command" | "super"
    ) {
        return Err(format!("{name} 必须以一个非修饰键结尾"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn existing_yaml_preserves_settings_when_display_limit_is_missing() {
        let source = DEFAULT_CONFIG
            .replace("  display_limit: 20\n", "")
            .replace("max_items: 100", "max_items: 321");
        let config: Config = serde_yaml::from_str(&source).unwrap();
        assert_eq!(config.history.display_limit, 20);
        assert_eq!(config.history.max_items, 321);
    }

    fn default_config() -> Config {
        serde_yaml::from_str(DEFAULT_CONFIG).expect("default YAML must deserialize")
    }

    #[test]
    fn saving_preserves_comments_and_round_trips_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        let mut config = Config::load(&path).unwrap();
        config.hotkeys.toggle = "Ctrl+Shift+V".into();
        config.appearance.font_family = "Inter, \"Microsoft YaHei UI\", sans-serif".into();
        config.appearance.accent_color = "#112233".into();
        config.history.max_items = 777;
        config.save(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("# 剪藏配置"));
        assert!(text.contains("toggle: \"Ctrl+Shift+V\" # 剪贴板历史"));
        assert!(text.contains("accent_color: \"#112233\" # 选中标记"));
        assert_eq!(Config::load(&path).unwrap(), config);

        // 旧文件缺少字段时回退到默认模板，仍然保存成功。
        fs::write(&path, DEFAULT_CONFIG.replace("  display_limit: 20\n", "")).unwrap();
        config.history.display_limit = 33;
        config.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), config);

        config.history.max_items = 0;
        assert!(config.save(&path).is_err());
    }

    #[test]
    fn validation_enforces_resource_and_shortcut_boundaries() {
        let mut config = default_config();
        config.history.max_items = 1;
        config.history.max_image_mb = 128;
        config.history.max_text_kb = 1_024;
        assert!(config.validate().is_ok());

        config.history.max_items = 0;
        assert!(config.validate().is_err());
        config = default_config();
        config.history.max_image_mb = 129;
        assert!(config.validate().is_err());
        config = default_config();
        config.history.max_text_kb = 1_025;
        assert!(config.validate().is_err());

        config = default_config();
        config.hotkeys.paste = "Ctrl++V".into();
        assert!(config.validate().is_err());
        config.hotkeys.paste = "Control+Ctrl+V".into();
        assert!(config.validate().is_err());
        config.hotkeys.paste = "Ctrl+Shift".into();
        assert!(config.validate().is_err());
    }
}
