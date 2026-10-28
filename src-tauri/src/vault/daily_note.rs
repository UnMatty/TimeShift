//! Генерация дневного фокус-лога «⏱ Фокус-логи/ГГГГ-ММ-ДД.md»
//! (v0.2, ARCHITECTURE §5.2; секция «🔎 Обзор» добавится в v0.3 с LLM).

use chrono::NaiveDate;

use crate::timer::{SessionStatus, Snapshot};

/// Путь заметки в хранилище (верхний уровень, рядом с «☀️ Сегодня.md»).
pub fn daily_note_path(day: NaiveDate) -> String {
    format!("⏱ Фокус-логи/{day}.md")
}

pub fn generate(snap: &Snapshot) -> String {
    let mut out = String::new();

    // --- Сводка ------------------------------------------------------------
    let completed = count(&snap.sessions, SessionStatus::Completed);
    let partial = count(&snap.sessions, SessionStatus::Partial);
    let voided = count(&snap.sessions, SessionStatus::Void);
    let focus_min: i64 = snap
        .sessions
        .iter()
        .filter(|s| matches!(s.status, SessionStatus::Completed | SessionStatus::Partial))
        .filter_map(|s| s.actual_min)
        .sum();
    let in_section = |wanted: Option<&str>| -> usize {
        snap.sessions
            .iter()
            .filter(|s| !matches!(s.status, SessionStatus::Void))
            .filter(|s| category_name(s.category) == wanted)
            .count()
    };
    let undefined = in_section(None);

    out.push_str(&format!(
        "---\ntags: [фокус-лог]\n---\n# ⏱ {day} — Фокус-лог\n\n> План дня: [[☀️ Сегодня]]\n\n## Сводка\n",
        day = snap.day
    ));
    out.push_str(&format!(
        "- Помидорок: {} (завершено {}, частично {}, аннулировано {}) — чистый фокус {}\n",
        completed + partial + voided,
        completed,
        partial,
        voided,
        fmt_hours(focus_min),
    ));
    out.push_str(&format!(
        "- Работа {} / Личное {} / Встречи {} / не определено {}\n",
        in_section(Some("work")),
        in_section(Some("personal")),
        in_section(Some("meeting")),
        undefined,
    ));

    // Прерывания: паузы с причинами по всем сессиям.
    let mut reason_parts = Vec::new();
    for reason in ["external", "internal", "away"] {
        let (count, _) = pauses_stats(snap, reason);
        if count > 0 {
            reason_parts.push(format!("{count} {}", reason_label(reason)));
        }
    }
    let total_pauses: usize = snap.sessions.iter().map(|s| s.pauses.iter().filter(|p| p.reason.is_some()).count()).sum();
    if total_pauses > 0 {
        out.push_str(&format!("- Прерываний: {} ({}) — {}\n", total_pauses, reason_parts.join(", "), fmt_hours(pauses_minutes(snap))));
    }
    out.push_str(&format!(
        "- Банк отдыха: сгорело {} мин{}\n",
        snap.burned_bank_min,
        if snap.day_closed { "" } else { " (день ещё открыт)" }
    ));

    // --- Сессии ------------------------------------------------------------
    let mut sessions: Vec<_> = snap.sessions.iter().collect();
    sessions.sort_by_key(|s| s.started_at);
    if !sessions.is_empty() {
        out.push_str("\n## Сессии\n| # | Время | Задача | Кат. | Заметка |\n|---|---|---|---|---|\n");
        for (i, s) in sessions.iter().enumerate() {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                i + 1,
                session_time(s),
                md(s.label.as_deref().unwrap_or("без подписи")),
                s.category.map(category_label).unwrap_or("—"),
                md(&s.note),
            ));
        }
    }

    // --- Прерывания и разрывы ---------------------------------------------
    let mut lines = Vec::new();
    for s in &snap.sessions {
        for p in &s.pauses {
            let Some(reason) = p.reason else { continue };
            let minutes = p.ended_at.map(|e| (e - p.started_at).num_minutes().max(1)).unwrap_or(0);
            lines.push(format!(
                "- {} {} ({} мин)",
                p.started_at.format("%H:%M"),
                reason_label(reason_name(reason)),
                minutes,
            ));
        }
    }
    for g in &snap.gaps {
        lines.push(format!(
            "- {}–{} разрыв: {}",
            g.started_at.format("%H:%M"),
            g.ended_at.format("%H:%M"),
            md(&g.reason),
        ));
    }
    if !lines.is_empty() {
        out.push_str("\n## Прерывания и разрывы\n");
        out.push_str(&lines.join("\n"));
        out.push('\n');
    }

    // --- Парковка -----------------------------------------------------------
    if !snap.thoughts.is_empty() {
        let resolved: Vec<_> = snap.thoughts.iter().filter(|t| t.resolved).collect();
        let open: Vec<_> = snap.thoughts.iter().filter(|t| !t.resolved).collect();
        out.push_str("\n## Парковка\n");
        for t in resolved {
            out.push_str(&format!("- разобрано: {}\n", md(&t.text)));
        }
        for t in open {
            out.push_str(&format!("- не разобрано: {}\n", md(&t.text)));
        }
    }

    // --- План/факт -----------------------------------------------------------
    if let Some(plan) = &snap.plan
        && !plan.tasks.is_empty()
    {
        out.push_str("\n## План/факт\n| Задача | Оценка | Факт | Δ |\n|---|---|---|---|\n");
        for t in &plan.tasks {
            let est = t.est_pomodoros.map(|e| e.to_string()).unwrap_or_else(|| "—".into());
            let delta = t
                .est_pomodoros
                .map(|e| (t.fact_pomodoros as i64 - e as i64).to_string())
                .unwrap_or_else(|| "—".into());
            out.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                md(&t.title),
                est,
                t.fact_pomodoros,
                delta,
            ));
        }
    }

    out
}

// --- вспомогательные --------------------------------------------------------

use crate::timer::UiSession;
use crate::wal::event::{Category, PauseReason};

fn count(sessions: &[UiSession], status: SessionStatus) -> usize {
    sessions.iter().filter(|s| s.status == status).count()
}

fn category_name(c: Option<Category>) -> Option<&'static str> {
    match c {
        Some(Category::Work) => Some("work"),
        Some(Category::Personal) => Some("personal"),
        Some(Category::Meeting) => Some("meeting"),
        None => None,
    }
}

fn category_label(c: Category) -> &'static str {
    match c {
        Category::Work => "работа",
        Category::Personal => "личное",
        Category::Meeting => "встреча",
    }
}

fn reason_name(r: PauseReason) -> &'static str {
    match r {
        PauseReason::Internal => "internal",
        PauseReason::External => "external",
        PauseReason::Away => "away",
    }
}

fn reason_label(code: &str) -> &'static str {
    match code {
        "internal" => "внутреннее",
        "external" => "внешнее",
        _ => "отошёл",
    }
}

fn pauses_stats(snap: &Snapshot, reason: &str) -> (usize, i64) {
    let mut count = 0;
    let mut minutes = 0;
    for s in &snap.sessions {
        for p in &s.pauses {
            let Some(r) = p.reason else { continue };
            if reason_name(r) != reason {
                continue;
            }
            count += 1;
            minutes += p.ended_at.map(|e| (e - p.started_at).num_minutes().max(1)).unwrap_or(0);
        }
    }
    (count, minutes)
}

fn pauses_minutes(snap: &Snapshot) -> i64 {
    snap.sessions
        .iter()
        .flat_map(|s| s.pauses.iter())
        .filter_map(|p| p.ended_at.map(|e| (e - p.started_at).num_minutes().max(1)))
        .sum()
}

fn session_time(s: &UiSession) -> String {
    let start = s.started_at.format("%H:%M");
    match s.ended_at {
        Some(end) => format!("{}–{}", start, end.format("%H:%M")),
        None => format!("{start}…"),
    }
}

/// «195» → «3ч 15м», «40» → «40м».
fn fmt_hours(minutes: i64) -> String {
    if minutes >= 60 {
        format!("{}ч {:02}м", minutes / 60, minutes % 60)
    } else {
        format!("{minutes}м")
    }
}

/// Внутритабличные `|` и переносы строк экранируем — заметка должна читаться агентом.
fn md(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timer::Timer;
    use crate::wal::event::{PlanSection, PlanTask};
    use chrono::{Local, TimeZone};
    use uuid::Uuid;

    fn snap_with_day() -> Snapshot {
        let mut t = Timer::new(crate::config::TimerConfig::default());
        let base = Local.with_ymd_and_hms(2026, 10, 28, 9, 0, 0).single().expect("время");
        let task = PlanTask {
            id: Uuid::new_v4(),
            section: PlanSection::Work,
            title: "Отчёты по команде".into(),
            wiki_links: vec![],
            marker: None,
            today_note_line: None,
            est_pomodoros: Some(2),
            done: false,
            today_note_done: false,
        };
        let task_id = task.id;
        let events = t.op_build_plan(base.date_naive(), vec![task], None).unwrap();
        for e in events {
            t.fold(&crate::wal::event::WalRecord { id: Uuid::new_v4(), ts: base, event: e });
        }
        let events = t
            .op_start_focus(base, Some("Отчёты по команде".into()), false, None, false, None, Some(task_id))
            .unwrap();
        for e in events {
            t.fold(&crate::wal::event::WalRecord { id: Uuid::new_v4(), ts: base, event: e });
        }
        let done = base + chrono::Duration::minutes(25);
        let events = t.tick(done);
        for e in events {
            t.fold(&crate::wal::event::WalRecord { id: Uuid::new_v4(), ts: done, event: e });
        }
        t.op_set_note(t.sessions[0].id, "профили созданы | осталось 3".into()).unwrap();
        let events = t.op_set_note(t.sessions[0].id, "профили созданы | осталось 3".into()).unwrap();
        for e in events {
            t.fold(&crate::wal::event::WalRecord { id: Uuid::new_v4(), ts: done, event: e });
        }
        let closed = done + chrono::Duration::minutes(6);
        let events = t.op_close_day().unwrap();
        for e in events {
            t.fold(&crate::wal::event::WalRecord { id: Uuid::new_v4(), ts: closed, event: e });
        }
        t.snapshot(closed)
    }

    #[test]
    fn note_follows_template() {
        let note = generate(&snap_with_day());
        assert!(note.starts_with("---\ntags: [фокус-лог]\n---\n# ⏱ 2026-10-28 — Фокус-лог"));
        assert!(note.contains("> План дня: [[☀️ Сегодня]]"));
        assert!(note.contains("## Сводка"));
        assert!(note.contains("Помидорок: 1 (завершено 1, частично 0, аннулировано 0) — чистый фокус 25м"));
        assert!(note.contains("Работа 0 / Личное 0 / Встречи 0 / не определено 1"), "категория в тесте не ставилась");
        assert!(note.contains("Банк отдыха: сгорело 0 мин"));
        assert!(note.contains("## Сессии"));
        assert!(note.contains("| 1 | 09:00–09:25 | Отчёты по команде | — | профили созданы \\| осталось 3 |"));
        assert!(note.contains("## План/факт"));
        assert!(note.contains("| Отчёты по команде | 2 | 1 | -1 |"));
        assert!(!note.contains("🔎 Обзор"), "обзор появится в v0.3");
    }

    #[test]
    fn daily_note_path_is_top_level() {
        let day = NaiveDate::from_ymd_opt(2026, 10, 28).unwrap();
        assert_eq!(daily_note_path(day), "⏱ Фокус-логи/2026-10-28.md");
    }
}
