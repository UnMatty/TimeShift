//! Парсер «☀️ Сегодня.md» (ARCHITECTURE §5.1, PRD FR-15).
//!
//! Терпим к структуре: берём все чекбоксы `^- \[( |x)\]` из любых секций, кроме
//! содержащих «встреч»/«дедлайн» (там не-чекбоксные упоминания). Из строки —
//! состояние, маркер (⏫/▲/⚡/🌱), текст, `[[wiki-ссылки]]`, дата в «(до …)».
//! Секция плана — предположение: заголовок «Рабочие…» → работа, остальное →
//! личное; владелец правит раскладку утром (это и есть планирование).
//! Ноль найденных чекбоксов — не ошибка парсера: вызывающий код обязан показать
//! предупреждение (риск «парсер сломался», ARCH §9).

use serde::Serialize;

use crate::wal::event::PlanSection;

/// Задача, извлечённая из строки «☀️ Сегодня».
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct TodayTask {
    /// Точная строка заметки — по ней выполняется обратная точечная замена
    /// чекбокса `[ ]`→`[x]` (привязка по тексту строки, а не по номеру).
    pub raw_line: String,
    pub done: bool,
    /// ⏫/▲/⚡/🌱, если стоит.
    pub marker: Option<String>,
    /// Текст задачи без чекбокса и маркера (ссылки и даты сохранены как есть).
    pub title: String,
    /// Цели `[[ссылок]]` (до `|` — без алиаса).
    pub wiki_links: Vec<String>,
    /// Содержимое «(до …)», если есть.
    pub due: Option<String>,
    /// Предположение о секции плана.
    pub section: PlanSection,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct TodayNote {
    pub tasks: Vec<TodayTask>,
}

impl TodayNote {
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }
}

pub fn parse_today(text: &str) -> TodayNote {
    let mut tasks = Vec::new();
    let mut header = String::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(h) = trimmed.strip_prefix("## ") {
            header = h.trim().to_string();
            continue;
        }
        let Some((done, body)) = checkbox_body(trimmed) else { continue };
        if excluded_section(&header) {
            continue;
        }
        let (marker, title) = split_marker(body);
        tasks.push(TodayTask {
            raw_line: line.trim_end().to_string(),
            done,
            marker,
            title: title.trim().to_string(),
            wiki_links: extract_wiki_links(title),
            due: extract_due(title),
            section: guess_section(&header),
        });
    }
    TodayNote { tasks }
}

/// `- [ ] текст` / `- [x] текст` → (готово, текст). Остальные строки — None.
fn checkbox_body(line: &str) -> Option<(bool, &str)> {
    let rest = line.strip_prefix("- [")?;
    let (state, after) = rest.split_once(']')?;
    match state {
        " " => Some((false, after)),
        "x" | "X" => Some((true, after)),
        _ => None,
    }
}

/// Секции-исключения: «Встречи», «Дедлайны» и производные (ARCH §5.1).
fn excluded_section(header: &str) -> bool {
    let h = header.to_lowercase();
    h.contains("встреч") || h.contains("дедлайн")
}

/// «Рабочие…» → работа, всё остальное (и строки до первого заголовка) → личное.
fn guess_section(header: &str) -> PlanSection {
    if header.to_lowercase().starts_with("рабочие") {
        PlanSection::Work
    } else {
        PlanSection::Personal
    }
}

const MARKERS: [char; 4] = ['⏫', '▲', '⚡', '🌱'];

/// Отделяет приоритетный маркер от текста задачи.
fn split_marker(body: &str) -> (Option<String>, &str) {
    let trimmed = body.trim_start();
    let mut chars = trimmed.chars();
    match chars.next() {
        Some(c) if MARKERS.contains(&c) => {
            let marker = c.to_string();
            let rest = chars.as_str().trim_start();
            (Some(marker), rest)
        }
        _ => (None, trimmed),
    }
}

fn extract_wiki_links(text: &str) -> Vec<String> {
    let mut links = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("[[") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("]]") else { break };
        let inner = &after[..end];
        let target = inner.split('|').next().unwrap_or(inner).trim();
        if !target.is_empty() {
            links.push(target.to_string());
        }
        rest = &after[end + 2..];
    }
    links
}

fn extract_due(text: &str) -> Option<String> {
    let start = text.find("(до ")?;
    let after = &text[start + "(до ".len()..];
    let end = after.find(')')?;
    let inner = after[..end].trim();
    (!inner.is_empty()).then(|| inner.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"---
tags: [сегодня]
---
# ☀️ Сегодня — 2026-10-28 (ср)

## Личные MIT (1–3)
- [ ] ⏫ Александрия: формирование претензии [[08 Личное/Области/Дом и быт|Дом и быт]] (горит с 2026-09-28)
- [ ] ▲ Подготовиться к паре 09 [[08 Личное/Проекты/Преподавание|Преподавание]] (до сб 2026-10-31)

## Рабочие ⏫/▲ из [[07 Задачи/📋 Задачи|📋 Задачи]]
- [x] ▲ Разобрать баги с дашборда релизов вместе с командой [[03 Процессы/Тестирование|Тестирование]]
- [ ] ▲ Экшен по ID в activity для соседей [[03 Процессы/Процесс разработки|Процесс разработки]]

## 🌱 Развитие дня
- [ ] Закрыть хотя бы одну 🌱-задачу недели — пока ноль

## Встречи
- Дейлик команды (~10:55) — вчера: [[02 Встречи/заметки]]
- **Сегодня: встреча с аналитиками**

## Дедлайны
- сегодня, ср 2026-10-28 — разбор багов с дашборда; встреча с аналитиками

## Ежедневные ритуалы
- [ ] Прочитать 1 сайт из закладки «Прочитать» — рабочее
"#;

    #[test]
    fn parses_real_note_format() {
        let note = parse_today(SAMPLE);
        assert_eq!(note.tasks.len(), 6, "2 MIT + 2 рабочих + 1 развитие + 1 ритуал; Встречи/Дедлайны — мимо");
    }

    #[test]
    fn extracts_state_marker_title_links_due() {
        let note = parse_today(SAMPLE);
        let mit = &note.tasks[0];
        assert!(!mit.done);
        assert_eq!(mit.marker.as_deref(), Some("⏫"));
        assert!(mit.title.starts_with("Александрия"));
        assert_eq!(mit.wiki_links, vec!["08 Личное/Области/Дом и быт".to_string()]);
        // «(горит с …)» под контракт due не попадает — только «(до …)».
        assert_eq!(mit.due, None);
        assert_eq!(mit.section, PlanSection::Personal);
        assert_eq!(
            mit.raw_line,
            "- [ ] ⏫ Александрия: формирование претензии [[08 Личное/Области/Дом и быт|Дом и быт]] (горит с 2026-09-28)"
        );

        let with_due = &note.tasks[1];
        assert_eq!(with_due.due.as_deref(), Some("сб 2026-10-31"));
    }

    #[test]
    fn work_headers_become_work_section() {
        let note = parse_today(SAMPLE);
        let work: Vec<_> = note.tasks.iter().filter(|t| t.section == PlanSection::Work).collect();
        assert_eq!(work.len(), 2, "секция «Рабочие …»");
        assert_eq!(work[0].marker.as_deref(), Some("▲"));
    }

    #[test]
    fn done_state_and_markerless_lines() {
        let note = parse_today(SAMPLE);
        let done: Vec<_> = note.tasks.iter().filter(|t| t.done).collect();
        assert_eq!(done.len(), 1);
        assert!(done[0].raw_line.contains("- [x]"));

        let ritual = note.tasks.last().expect("ритуал");
        assert_eq!(ritual.marker, None, "без маркера");
        assert_eq!(ritual.section, PlanSection::Personal);
    }

    #[test]
    fn empty_note_is_empty_not_error() {
        let note = parse_today("# ☀️ Сегодня\n\nПусто, агент ещё не собрал.\n");
        assert!(note.is_empty());
    }

    #[test]
    fn quasi_checkboxes_are_ignored() {
        let note = parse_today("## Х\n- [q] не чекбокс\n- [X] заглавная X — тоже готово\n");
        assert_eq!(note.tasks.len(), 1, "[q] — не чекбокс; заглавная [X] принимается");
        assert!(note.tasks[0].done);
    }
}
