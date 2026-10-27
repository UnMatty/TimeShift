//! Append-only журнал событий (WAL) — ARCHITECTURE §3.2.
//!
//! Правило надёжности приложения: любое событие сначала сюда, потом куда угодно.
//! Файл на локальную дату: `~/.timeshift/wal/ГГГГ-ММ-ДД.jsonl`, одна строка — одна
//! запись `WalRecord`. После каждой записи — `sync_data` (переживает и обрыв
//! процесса, и потерю питания). Читатель терпим к оборванной последней строке:
//! процесс мог умереть посреди записи — такая строка пропускается и считается.

pub mod event;
#[cfg(test)]
mod tests;

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local, NaiveDate};

use crate::wal::event::{Event, WalRecord};

#[derive(Debug, thiserror::Error)]
pub enum WalError {
    #[error("ошибка ввода-вывода: {0}")]
    Io(#[from] io::Error),
    #[error("не удалось разобрать запись журнала: {0}")]
    Json(#[from] serde_json::Error),
}

/// Серия записей в одном файле до переключения на новую дату.
struct CurrentFile {
    date: NaiveDate,
    file: File,
}

pub struct Wal {
    dir: PathBuf,
    current: Option<CurrentFile>,
}

/// Результат проигрывания журнала.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Playback {
    /// Все разобранные записи, упорядоченные по `ts` (ничьи — в порядке файл/строка).
    pub events: Vec<WalRecord>,
    /// Строки, которые не разобрались (обычно оборванный хвост после падения процесса).
    pub skipped_lines: usize,
    /// Файлы в каталоге без формата `ГГГГ-ММ-ДД.jsonl`.
    pub skipped_files: usize,
}

impl Wal {
    /// Открывает (и создаёт) каталог журнала по указанному пути.
    pub fn open(dir: impl Into<PathBuf>) -> io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        Ok(Wal {
            dir,
            current: None,
        })
    }

    /// Каталог по умолчанию: `~/.timeshift/wal`.
    pub fn open_default() -> io::Result<Self> {
        let home = dirs::home_dir().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "не найден домашний каталог")
        })?;
        Self::open(home.join(".timeshift").join("wal"))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Штампует событие и дописывает в файл текущей локальной даты.
    pub fn append(&mut self, event: impl Into<Event>) -> Result<WalRecord, WalError> {
        let record = WalRecord::now(event);
        let date = record.ts.date_naive();
        self.append_record(date, record)
    }

    /// Тестовый вход: та же запись, но с навязанной датой файла — проверяет
    /// переключение файла при смене суток.
    #[doc(hidden)]
    pub fn append_on(
        &mut self,
        date: NaiveDate,
        ts: DateTime<Local>,
        event: impl Into<Event>,
    ) -> Result<WalRecord, WalError> {
        let record = WalRecord {
            id: uuid::Uuid::new_v4(),
            ts,
            event: event.into(),
        };
        self.append_record(date, record)
    }

    fn append_record(&mut self, date: NaiveDate, record: WalRecord) -> Result<WalRecord, WalError> {
        // Весь буфер (строка + перевод) — за один write_all: чем меньше окно
        // между началом записи и fsync, тем короче возможный оборванный хвост.
        let mut line = serde_json::to_vec(&record)?;
        line.push(b'\n');
        let file = self.file_for(date)?;
        file.write_all(&line)?;
        file.sync_data()?;
        Ok(record)
    }

    fn file_for(&mut self, date: NaiveDate) -> io::Result<&mut File> {
        if !matches!(&self.current, Some(c) if c.date == date) {
            let path = self.dir.join(format!("{date}.jsonl"));
            let file = OpenOptions::new().create(true).append(true).open(path)?;
            self.current = Some(CurrentFile { date, file });
        }
        Ok(&mut self.current.as_mut().expect("файл только что установлен").file)
    }
}

/// Проигрывает весь журнал: все даты, отсортировано по `ts`.
pub fn read_all(dir: impl AsRef<Path>) -> Result<Playback, WalError> {
    let mut dates: Vec<NaiveDate> = Vec::new();
    let mut skipped_files = 0;
    for entry in fs::read_dir(dir.as_ref())? {
        let entry = entry?;
        let name = entry.file_name();
        let lossy = name.to_string_lossy();
        let Some(stem) = lossy.strip_suffix(".jsonl") else {
            skipped_files += 1;
            continue;
        };
        match NaiveDate::parse_from_str(stem, "%Y-%m-%d") {
            Ok(date) => dates.push(date),
            Err(_) => skipped_files += 1,
        }
    }
    dates.sort();

    let mut playback = Playback {
        skipped_files,
        ..Playback::default()
    };
    let mut dated: Vec<(NaiveDate, usize, WalRecord)> = Vec::new();
    for date in dates {
        read_file(&dir.as_ref().join(format!("{date}.jsonl")), date, &mut dated, &mut playback.skipped_lines)?;
    }
    dated.sort_by_key(|a| a.2.ts);
    playback.events = dated.into_iter().map(|(_, _, r)| r).collect();
    Ok(playback)
}

/// Проигрывает один день.
pub fn read_day(dir: impl AsRef<Path>, day: NaiveDate) -> Result<Playback, WalError> {
    let path = dir.as_ref().join(format!("{day}.jsonl"));
    if !path.exists() {
        return Ok(Playback::default());
    }
    let mut dated = Vec::new();
    let mut skipped_lines = 0;
    read_file(&path, day, &mut dated, &mut skipped_lines)?;
    dated.sort_by_key(|a| a.2.ts);
    Ok(Playback {
        events: dated.into_iter().map(|(_, _, r)| r).collect(),
        skipped_lines,
        skipped_files: 0,
    })
}

fn read_file(
    path: &Path,
    date: NaiveDate,
    out: &mut Vec<(NaiveDate, usize, WalRecord)>,
    skipped_lines: &mut usize,
) -> Result<(), WalError> {
    let mut buf = String::new();
    BufReader::new(File::open(path)?).read_to_string(&mut buf)?;
    for (idx, line) in buf.lines().enumerate() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<WalRecord>(line) {
            Ok(record) => out.push((date, idx, record)),
            Err(_) => *skipped_lines += 1,
        }
    }
    Ok(())
}
