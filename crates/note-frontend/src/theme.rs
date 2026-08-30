use serde::{Deserialize, Serialize};

pub const THEME_STORAGE_KEY: &str = "agent-note-theme";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    #[default]
    Auto,
    Light,
    Dark,
}

impl ThemeMode {
    pub const ALL: [Self; 3] = [Self::Auto, Self::Light, Self::Dark];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }

    pub fn data_theme(self) -> Option<&'static str> {
        match self {
            Self::Auto => None,
            Self::Light => Some("sunshine"),
            Self::Dark => Some("moonlight"),
        }
    }

    pub fn from_str(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "light" => Self::Light,
            "dark" => Self::Dark,
            _ => Self::Auto,
        }
    }
}

pub fn get_saved_theme() -> ThemeMode {
    #[cfg(target_arch = "wasm32")]
    {
        if let Some(window) = web_sys::window() {
            if let Ok(Some(storage)) = window.local_storage() {
                if let Ok(Some(value)) = storage.get_item(THEME_STORAGE_KEY) {
                    return ThemeMode::from_str(&value);
                }
            }
        }
    }
    ThemeMode::Auto
}

pub fn apply_theme(mode: ThemeMode) {
    #[cfg(target_arch = "wasm32")]
    {
        if let Some(window) = web_sys::window() {
            if let Ok(Some(storage)) = window.local_storage() {
                let _ = storage.set_item(THEME_STORAGE_KEY, mode.as_str());
            }
            if let Some(document) = window.document() {
                if let Some(html) = document.document_element() {
                    match mode.data_theme() {
                        Some(theme_name) => {
                            let _ = html.set_attribute("data-theme", theme_name);
                        }
                        None => {
                            let _ = html.remove_attribute("data-theme");
                        }
                    }
                }
            }
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = mode;
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_mode_converts_to_and_from_str() {
        assert_eq!(ThemeMode::from_str("auto"), ThemeMode::Auto);
        assert_eq!(ThemeMode::from_str("Auto"), ThemeMode::Auto);
        assert_eq!(ThemeMode::from_str("light"), ThemeMode::Light);
        assert_eq!(ThemeMode::from_str("LIGHT"), ThemeMode::Light);
        assert_eq!(ThemeMode::from_str("dark"), ThemeMode::Dark);
        assert_eq!(ThemeMode::from_str("Dark"), ThemeMode::Dark);
        assert_eq!(ThemeMode::from_str("unknown"), ThemeMode::Auto);
        assert_eq!(ThemeMode::from_str(""), ThemeMode::Auto);

        assert_eq!(ThemeMode::Auto.as_str(), "auto");
        assert_eq!(ThemeMode::Light.as_str(), "light");
        assert_eq!(ThemeMode::Dark.as_str(), "dark");

        assert_eq!(ThemeMode::Auto.label(), "Auto");
        assert_eq!(ThemeMode::Light.label(), "Light");
        assert_eq!(ThemeMode::Dark.label(), "Dark");
    }

    #[test]
    fn theme_mode_data_theme_mapping() {
        assert_eq!(ThemeMode::Auto.data_theme(), None);
        assert_eq!(ThemeMode::Light.data_theme(), Some("sunshine"));
        assert_eq!(ThemeMode::Dark.data_theme(), Some("moonlight"));
    }

    #[test]
    fn theme_mode_serde_roundtrip() {
        let serialized = serde_json::to_string(&ThemeMode::Light).unwrap();
        assert_eq!(serialized, "\"light\"");
        let deserialized: ThemeMode = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, ThemeMode::Light);
    }
}
