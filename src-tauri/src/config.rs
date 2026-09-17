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
