use rusqlite::{Connection, OpenFlags};

const LANGUAGES: [&str; 4] = ["zh-CN", "zh-TW", "en-US", "ja-JP"];

pub fn preferred_language() -> String {
    if let Some(language) = stored_language() {
        if LANGUAGES.contains(&language.as_str()) {
            return language;
        }
    }
    normalize_locale(&system_locale())
}

fn stored_language() -> Option<String> {
    let path = dirs::data_dir()?.join("NodeSend").join("nodesend.db");
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    connection
        .query_row(
            "SELECT value FROM settings WHERE key='ui_language'",
            [],
            |row| row.get(0),
        )
        .ok()
}

pub fn normalize_locale(locale: &str) -> String {
    let value = locale.replace('_', "-").to_ascii_lowercase();
    if value.starts_with("zh-hant")
        || ["zh-tw", "zh-hk", "zh-mo"]
            .iter()
            .any(|tag| value.starts_with(tag))
    {
        "zh-TW".into()
    } else if value.starts_with("zh") {
        "zh-CN".into()
    } else if value.starts_with("en") {
        "en-US".into()
    } else if value.starts_with("ja") {
        "ja-JP".into()
    } else {
        "zh-CN".into()
    }
}

#[cfg(windows)]
fn system_locale() -> String {
    const BUFFER_SIZE: usize = 85;
    let mut buffer = [0u16; BUFFER_SIZE];
    let length = unsafe { GetUserDefaultLocaleName(buffer.as_mut_ptr(), BUFFER_SIZE as i32) };
    if length <= 1 {
        return "zh-CN".into();
    }
    String::from_utf16_lossy(&buffer[..(length - 1) as usize])
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetUserDefaultLocaleName(locale_name: *mut u16, count: i32) -> i32;
}

#[cfg(not(windows))]
fn system_locale() -> String {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .filter_map(|key| std::env::var(key).ok())
        .find(|value| !value.is_empty())
        .unwrap_or_else(|| "zh-CN".into())
}

#[cfg(test)]
mod tests {
    use super::normalize_locale;

    #[test]
    fn locale_tags_map_to_supported_languages() {
        assert_eq!(normalize_locale("zh-CN"), "zh-CN");
        assert_eq!(normalize_locale("zh-HK"), "zh-TW");
        assert_eq!(normalize_locale("zh-Hant-TW"), "zh-TW");
        assert_eq!(normalize_locale("en-GB"), "en-US");
        assert_eq!(normalize_locale("ja-JP"), "ja-JP");
        assert_eq!(normalize_locale("de-DE"), "zh-CN");
    }
}
