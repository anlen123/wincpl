/// Windows OCR separates Chinese words with spaces. Preserve Latin word boundaries
/// while joining CJK characters and punctuation for natural substring searches.
pub fn normalize_text(text: &str) -> String {
    fn cjk(c: char) -> bool {
        matches!(c as u32, 0x2E80..=0x303F | 0x3040..=0x30FF | 0x3400..=0x9FFF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF | 0x20000..=0x323AF)
            || matches!(c, '“' | '”' | '‘' | '’')
    }
    let mut result = String::with_capacity(text.len());
    let mut previous = None;
    let mut whitespace = false;
    for current in text.chars() {
        if current.is_whitespace() {
            whitespace = true;
            continue;
        }
        if whitespace && previous.is_some_and(|prev| !(cjk(prev) && cjk(current))) {
            result.push(' ');
        }
        result.push(current);
        previous = Some(current);
        whitespace = false;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chinese_ocr_words_are_searchable_without_losing_latin_boundaries() {
        let text = normalize_text("第 二 步 ： 如 何 把 应 用 扔 进 这 个 “ 跨 屏 大 格 子 ”");
        assert_eq!(text, "第二步：如何把应用扔进这个“跨屏大格子”");
        assert!(text.contains("跨屏"));
        assert_eq!(
            normalize_text("  中文 Windows 11\nhello world  "),
            "中文 Windows 11 hello world"
        );
    }
}
