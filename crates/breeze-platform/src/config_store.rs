//! Loads the user config file (`~/.config/breeze/config`) and parses it with
//! the core config parser. Absent file → defaults.

use breeze_core::config::BreezeConfig;
use std::path::{Path, PathBuf};

/// `~/.config/breeze/config`.
pub fn config_path() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join(".config/breeze/config")
}

/// Load + parse the user config. Returns defaults if the file is absent or
/// unreadable.
pub fn load() -> BreezeConfig {
    load_from(&config_path())
}

pub fn load_from(path: &Path) -> BreezeConfig {
    match std::fs::read_to_string(path) {
        Ok(text) => BreezeConfig::parse(&text),
        Err(_) => BreezeConfig::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use breeze_core::config::{CursorStyle, Rgba};

    fn tmp() -> PathBuf {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("breeze-cfg-{}-{}/config", std::process::id(), n))
    }

    #[test]
    fn absent_file_is_default() {
        let cfg = load_from(Path::new("/no/such/breeze/config"));
        assert_eq!(cfg, BreezeConfig::default());
    }

    #[test]
    fn reads_and_parses_real_file() {
        let path = tmp();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "# my config\nfont-size = 16\nbackground = #101418\ncursor-style = bar\ncopy-on-select = yes\n",
        )
        .unwrap();
        let cfg = load_from(&path);
        assert_eq!(cfg.font_size, Some(16.0));
        assert_eq!(cfg.background, Some(Rgba { r: 0x10, g: 0x14, b: 0x18, a: 255 }));
        assert_eq!(cfg.cursor_style, Some(CursorStyle::SteadyBar));
        assert!(cfg.copy_on_select);
    }
}
