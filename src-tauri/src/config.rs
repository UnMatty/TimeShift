//! Конфиг приложения: `~/.timeshift/config.toml` (ARCHITECTURE §10).
//!
//! Секреты (пароль БД, будущие ключи vault/LLM) живут только в этом файле
//! и Keychain — в репозиторий не попадают. Все поля, кроме `db.url`,
//! имеют осмысленные значения по умолчанию.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub db: DbConfig,
    #[serde(default)]
    pub timer: TimerConfig,
    #[serde(default)]
    pub vault: VaultConfig,
    #[serde(default)]
    pub llm: LlmConfig,
    #[serde(default)]
    pub app: AppConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbConfig {
    #[serde(default = "default_db_url")]
    pub url: String,
}

impl Default for DbConfig {
    fn default() -> Self {
        Self { url: default_db_url() }
    }
}

fn default_db_url() -> String {
    "postgresql://timeshift@localhost:5432/timeshift".into()
}

/// Длительности помидорки (PRD FR-1): классика 25/5/15, длинный после 4-х.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TimerConfig {
    #[serde(default = "default_focus")]
    pub focus_min: i64,
    #[serde(default = "default_short")]
    pub short_break_min: i64,
    #[serde(default = "default_long")]
    pub long_break_min: i64,
    #[serde(default = "default_long_every")]
    pub long_every: u32,
    /// Порог разрыва между этапами для ретро-вопроса (PRD FR-4).
    #[serde(default = "default_gap")]
    pub gap_threshold_min: i64,
}

impl Default for TimerConfig {
    fn default() -> Self {
        Self {
            focus_min: default_focus(),
            short_break_min: default_short(),
            long_break_min: default_long(),
            long_every: default_long_every(),
            gap_threshold_min: default_gap(),
        }
    }
}

fn default_focus() -> i64 { 25 }
fn default_short() -> i64 { 5 }
fn default_long() -> i64 { 15 }
fn default_long_every() -> u32 { 4 }
fn default_gap() -> i64 { 10 }

/// Подготовка к v0.2: Local REST API плагина Obsidian.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[derive(Default)]
pub struct VaultConfig {
    #[serde(default)]
    pub api_url: Option<String>,
}


/// Подготовка к v0.3: OpenAI-совместимый endpoint для вечернего обзора.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[derive(Default)]
pub struct LlmConfig {
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}


#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[derive(Default)]
pub struct AppConfig {
    #[serde(default)]
    pub autostart: bool,
}


impl Config {
    pub fn path() -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(".timeshift").join("config.toml"))
    }

    /// Читает конфиг; отсутствующий файл заменяется дефолтами (запись — через `save`).
    pub fn load() -> Self {
        let Some(path) = Self::path() else { return Self::default() };
        match std::fs::read_to_string(&path) {
            Ok(raw) => toml::from_str(&raw).unwrap_or_else(|e| {
                eprintln!("timeshift: не удалось разобрать {}: {e}; использую дефолты", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::path().ok_or("не найден домашний каталог")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let raw = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, raw).map_err(|e| e.to_string())
    }
}
