//! Конфиг приложения: `~/.timeshift/config.toml` (ARCHITECTURE §10).
//!
//! Секреты (пароль БД, ключи vault/LLM) живут только в этом файле
//! и Keychain — в репозиторий не попадают. Все поля имеют осмысленные
//! значения по умолчанию: свежая установка работает на WAL-файлах без
//! Docker и Obsidian (v0.3 «Модули»).
//!
//! Легаси-совместимость: конфиги до v0.3 имели только `[db] url`
//! (Postgres-реплика). Отсутствие `backend` при заданном `url`
//! трактуется как Postgres; явный `backend = "none"` выключает реплику.

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

/// Подключаемый бэкенд реплики WAL (v0.3). Сам WAL — всегда источник истины;
/// бэкенд лишь доставляет журнал в реляционное хранилище.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DbBackend {
    /// Только WAL-файлы, без реплики (свежая установка).
    #[default]
    None,
    /// Локальный SQLite-файл (`~/.timeshift/timeshift.db` по умолчанию).
    Sqlite,
    /// Postgres в существующем контейнере (как до v0.3).
    Postgres,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DbConfig {
    /// Явный выбор бэкенда. `None` (поле отсутствует) = легаси-режим:
    /// бэкенд выводится из `url` (для совместимости с конфигами до v0.3).
    #[serde(default)]
    pub backend: Option<DbBackend>,
    /// Postgres: строка подключения; SQLite: путь к файлу.
    #[serde(default)]
    pub url: Option<String>,
}

fn default_db_url() -> String {
    "postgresql://timeshift@localhost:5432/timeshift".into()
}

/// Путь SQLite-файла по умолчанию (рядом с WAL).
fn default_sqlite_path() -> String {
    dirs::home_dir()
        .map(|h| {
            h.join(".timeshift")
                .join("timeshift.db")
                .display()
                .to_string()
        })
        .unwrap_or_else(|| "timeshift.db".into())
}

impl DbConfig {
    /// Резолюция к фактическому бэкенду и строке подключения.
    /// Гарантии: Sqlite → url всегда задан (дефолт-путь); Postgres →
    /// url либо задан, либо легаси-дефолт.
    pub fn effective(&self) -> (DbBackend, Option<String>) {
        match self.backend {
            Some(DbBackend::None) => (DbBackend::None, None),
            Some(DbBackend::Sqlite) => (
                DbBackend::Sqlite,
                Some(self.url.clone().unwrap_or_else(default_sqlite_path)),
            ),
            Some(DbBackend::Postgres) => (
                DbBackend::Postgres,
                Some(self.url.clone().unwrap_or_else(default_db_url)),
            ),
            // Легаси: поле backend отсутствует. Конфиги до v0.3 имели только
            // url (Postgres-строка); url нет — свежая установка, работаем на файлах.
            None => match &self.url {
                Some(u) => (DbBackend::Postgres, Some(u.clone())),
                None => (DbBackend::None, None),
            },
        }
    }
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

fn default_focus() -> i64 {
    25
}
fn default_short() -> i64 {
    5
}
fn default_long() -> i64 {
    15
}
fn default_long_every() -> u32 {
    4
}
fn default_gap() -> i64 {
    10
}

/// Доступ к хранилищу Obsidian через Local REST API (v0.2, ARCHITECTURE §3.4).
/// Подключаемый модуль (v0.3): без ключей или с `enabled = false` приложение
/// полноценно работает без Obsidian.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultConfig {
    /// Тумблер модуля (v0.3). По умолчанию включён — но активность всё равно
    /// требует api_url/api_key; false выключает жёстко, даже при наличии ключей.
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub api_url: Option<String>,
    /// Bearer-ключ плагина. Прецедент — пароль БД в этом же конфиге; переезд
    /// в Keychain — отдельным шагом по решению владельца.
    #[serde(default)]
    pub api_key: Option<String>,
}

impl Default for VaultConfig {
    fn default() -> Self {
        // Derived Default дал бы enabled = false и выключил бы Obsidian
        // у всех существующих пользователей после первого сохранения конфига.
        Self {
            enabled: true,
            api_url: None,
            api_key: None,
        }
    }
}

impl VaultConfig {
    /// Модуль активен: включён и настроен.
    pub fn is_active(&self) -> bool {
        self.enabled && self.api_url.is_some() && self.api_key.is_some()
    }
}

fn default_true() -> bool {
    true
}

/// Подготовка к v0.3: OpenAI-совместимый endpoint для вечернего обзора.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LlmConfig {
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
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
        let Some(path) = Self::path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(raw) => toml::from_str(&raw).unwrap_or_else(|e| {
                eprintln!(
                    "timeshift: не удалось разобрать {}: {e}; использую дефолты",
                    path.display()
                );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_install_stays_on_files() {
        // Нет ни конфига, ни полей — работаем на WAL-файлах.
        let (b, u) = DbConfig::default().effective();
        assert_eq!(b, DbBackend::None);
        assert_eq!(u, None);
    }

    #[test]
    fn legacy_url_without_backend_means_postgres() {
        let cfg: DbConfig =
            toml::from_str(r#"url = "postgresql://timeshift@localhost:5432/timeshift""#)
                .expect("легаси-конфиг");
        let (b, u) = cfg.effective();
        assert_eq!(b, DbBackend::Postgres);
        assert_eq!(
            u.as_deref(),
            Some("postgresql://timeshift@localhost:5432/timeshift")
        );
    }

    #[test]
    fn explicit_none_overrides_url() {
        let cfg: DbConfig = toml::from_str(
            r#"backend = "none"
url = "postgresql://x""#,
        )
        .expect("явный none");
        let (b, u) = cfg.effective();
        assert_eq!(b, DbBackend::None);
        assert_eq!(u, None);
    }

    #[test]
    fn sqlite_gets_default_path() {
        let cfg: DbConfig = toml::from_str(r#"backend = "sqlite""#).expect("sqlite");
        let (b, u) = cfg.effective();
        assert_eq!(b, DbBackend::Sqlite);
        let path = u.expect("дефолт-путь");
        assert!(
            path.ends_with(".timeshift/timeshift.db"),
            "неожиданный путь: {path}"
        );
    }

    #[test]
    fn sqlite_keeps_explicit_path() {
        let cfg: DbConfig = toml::from_str(
            r#"backend = "sqlite"
url = "/tmp/my.db""#,
        )
        .expect("sqlite");
        let (_, u) = cfg.effective();
        assert_eq!(u.as_deref(), Some("/tmp/my.db"));
    }

    #[test]
    fn postgres_without_url_uses_legacy_default() {
        let cfg: DbConfig = toml::from_str(r#"backend = "postgres""#).expect("postgres");
        let (b, u) = cfg.effective();
        assert_eq!(b, DbBackend::Postgres);
        assert_eq!(
            u.as_deref(),
            Some("postgresql://timeshift@localhost:5432/timeshift")
        );
    }

    #[test]
    fn backend_roundtrips_through_toml() {
        let cfg: Config = toml::from_str(
            r#"[db]
backend = "sqlite"
url = "/tmp/x.db"

[vault]
enabled = false
"#,
        )
        .expect("round-trip");
        assert_eq!(cfg.db.backend, Some(DbBackend::Sqlite));
        assert!(!cfg.vault.enabled);
    }

    #[test]
    fn vault_defaults_to_enabled() {
        let cfg = VaultConfig::default();
        assert!(cfg.enabled);
        assert!(!cfg.is_active(), "без ключей не активен");
    }

    #[test]
    fn vault_disabled_even_with_keys() {
        let cfg: VaultConfig = toml::from_str(
            r#"enabled = false
api_url = "https://127.0.0.1:27124"
api_key = "secret""#,
        )
        .expect("выключенный vault");
        assert!(!cfg.is_active());
    }
}
