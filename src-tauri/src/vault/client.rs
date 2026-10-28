//! HTTP-клиент Local REST API плагина Obsidian (ARCHITECTURE §3.4).
//!
//! Три операции, которые нужны TimeShift: `read_note` (GET — свежее чтение перед
//! любой записью), `write_note` (PUT — полная перезапись известного содержимого:
//! фокус-лог, точечная замена чекбокса) и `append_note` (POST — дополнение в конец).

use std::time::Duration;

use reqwest::{Client, StatusCode, Url};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum VaultError {
    #[error("некорректный адрес заметки: {0}")]
    BadUrl(String),
    #[error("хранилище недоступно: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("хранилище вернуло статус {0}")]
    Status(StatusCode),
    #[error("заметки нет в хранилище: {0}")]
    NotFound(String),
}

pub struct VaultClient {
    http: Client,
    base: Url,
    api_key: String,
}

impl VaultClient {
    /// Сборка клиента из конфига. `danger_accept_invalid_certs` включается только
    /// для loopback-хостов (самоподписанный сертификат плагина, ARCH §3.4).
    pub fn new(api_url: &str, api_key: &str) -> Result<Self, String> {
        let mut base = Url::parse(api_url).map_err(|e| format!("некорректный api_url: {e}"))?;
        if !matches!(base.scheme(), "https" | "http") {
            return Err("api_url должен начинаться с http(s)://".into());
        }
        let host = base.host_str().ok_or("в api_url нет хоста")?.to_ascii_lowercase();
        let loopback = matches!(host.as_str(), "127.0.0.1" | "localhost" | "::1");
        let path = base.path().to_string();
        if !path.ends_with('/') {
            base.set_path(&format!("{path}/"));
        }

        let mut builder = Client::builder().timeout(Duration::from_secs(15));
        if loopback {
            builder = builder.danger_accept_invalid_certs(true);
        }
        let http = builder.build().map_err(|e| format!("HTTP-клиент: {e}"))?;
        Ok(Self { http, base, api_key: api_key.to_string() })
    }

    /// URL заметки: `{base}vault/{путь}` с percent-кодированием по сегментам —
    /// пути заметок содержат эмодзи, пробелы и кириллицу («☀️ Сегодня.md»),
    /// а разделитель `/` кодировать нельзя.
    pub fn note_url(&self, path: &str) -> Result<Url, VaultError> {
        let trimmed = path.trim().trim_start_matches('/');
        if trimmed.is_empty() {
            return Err(VaultError::BadUrl(path.into()));
        }
        let encoded = trimmed.split('/').map(urlencoding::encode).collect::<Vec<_>>().join("/");
        self.base
            .join(&format!("vault/{encoded}"))
            .map_err(|e| VaultError::BadUrl(e.to_string()))
    }

    fn request(&self, method: reqwest::Method, url: Url) -> reqwest::RequestBuilder {
        self.http
            .request(method, url)
            .header("Authorization", format!("Bearer {}", self.api_key))
    }

    /// Свежее чтение заметки (правило «перезапись только после чтения»).
    pub async fn read_note(&self, path: &str) -> Result<String, VaultError> {
        let response = self.request(reqwest::Method::GET, self.note_url(path)?).send().await?;
        match response.status() {
            StatusCode::NOT_FOUND => Err(VaultError::NotFound(path.into())),
            status if !status.is_success() => Err(VaultError::Status(status)),
            _ => Ok(response.text().await?),
        }
    }

    /// Полная перезапись заметки (создаёт файл, если его ещё нет).
    pub async fn write_note(&self, path: &str, content: &str) -> Result<(), VaultError> {
        let response = self
            .request(reqwest::Method::PUT, self.note_url(path)?)
            .header("Content-Type", "text/plain")
            .body(content.to_string())
            .send()
            .await?;
        match response.status() {
            status if status.is_success() => Ok(()),
            status => Err(VaultError::Status(status)),
        }
    }

    /// Дополнение в конец заметки (Local REST API: POST).
    pub async fn append_note(&self, path: &str, content: &str) -> Result<(), VaultError> {
        let response = self
            .request(reqwest::Method::POST, self.note_url(path)?)
            .header("Content-Type", "text/plain")
            .body(content.to_string())
            .send()
            .await?;
        match response.status() {
            status if status.is_success() => Ok(()),
            status => Err(VaultError::Status(status)),
        }
    }

    /// Живой ли плагин (используется индикатором «хранилище» в UI).
    pub async fn healthy(&self) -> bool {
        self.request(reqwest::Method::GET, self.base.clone())
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(url: &str) -> VaultClient {
        VaultClient::new(url, "test-key").expect("клиент")
    }

    #[test]
    fn note_url_encodes_emoji_and_cyrillic() {
        let c = client("https://127.0.0.1:27124");
        let url = c.note_url("☀️ Сегодня.md").expect("url");
        assert_eq!(url.as_str(), "https://127.0.0.1:27124/vault/%E2%98%80%EF%B8%8F%20%D0%A1%D0%B5%D0%B3%D0%BE%D0%B4%D0%BD%D1%8F.md");
    }

    #[test]
    fn nested_paths_are_preserved() {
        let c = client("https://127.0.0.1:27124/");
        let url = c.note_url("⏱ Фокус-логи/2026-10-28.md").expect("url");
        assert!(
            url.path().starts_with("/vault/") && url.path().ends_with("/2026-10-28.md"),
            "разделитель пути не должен кодироваться: {}",
            url.path()
        );
        assert_eq!(url.host_str(), Some("127.0.0.1"));
    }

    #[test]
    fn base_without_trailing_slash_still_works() {
        let c = client("https://127.0.0.1:27124");
        let url = c.note_url("x.md").expect("url");
        assert_eq!(url.path(), "/vault/x.md");
    }

    #[test]
    fn empty_path_is_rejected() {
        let c = client("https://127.0.0.1:27124");
        assert!(matches!(c.note_url("   "), Err(VaultError::BadUrl(_))));
        assert!(matches!(c.note_url("/"), Err(VaultError::BadUrl(_))));
    }

    #[test]
    fn invalid_urls_are_rejected() {
        assert!(VaultClient::new("ftp://127.0.0.1", "k").is_err());
        assert!(VaultClient::new("не url", "k").is_err());
    }
}
