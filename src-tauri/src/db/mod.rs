//! Репликатор WAL → подключаемый бэкенд (v0.3 «Модули», ARCHITECTURE §3.3).
//!
//! WAL — единственный источник истины; бэкенд только доставляет журнал в
//! реляционное хранилище (Postgres или SQLite-файл). Правило идемпотентности:
//! событие применяется в той же транзакции, что и вставка в `applied_events` —
//! повторный прогон журнала не создаёт дублей. Бэкенд недоступен — цикл молча
//! ретраит; в UI статус отдаётся через `db_online`. Для `DbBackend::None`
//! петля вообще не запускается: приложение живёт на WAL-файлах.

mod postgres;
mod sqlite;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use sqlx::{PgPool, SqlitePool};

pub use crate::config::DbBackend;

use crate::wal::event::{
    BreakAction, BreakKind, Category, PauseReason, PlanSection, SessionSource, ThoughtResolution,
};
use crate::wal::read_all;

/// Пул подключения к активному бэкенду.
pub enum Pool {
    Pg(PgPool),
    Sq(SqlitePool),
}

/// Подключается и применяет миграции (Postgres: `../migrations`,
/// SQLite: `../migrations-sqlite` — относительно src-tauri).
pub async fn connect(backend: DbBackend, url: &str) -> Result<Pool, String> {
    match backend {
        DbBackend::None => Err("реплика выключена (backend = none)".into()),
        DbBackend::Postgres => Ok(Pool::Pg(postgres::connect(url).await?)),
        DbBackend::Sqlite => Ok(Pool::Sq(sqlite::connect(url).await?)),
    }
}

/// Вечный цикл: подключение → репликация → при потере связи переподключение.
/// Для `DbBackend::None` не вызывается (lib.rs петлю не спавнит).
pub async fn replicator_loop(
    backend: DbBackend,
    url: String,
    wal_dir: PathBuf,
    online: Arc<AtomicBool>,
) {
    loop {
        match connect(backend, &url).await {
            Ok(pool) => {
                online.store(true, Ordering::Relaxed);
                match replicate_cycles(&wal_dir, &pool, &online).await {
                    Ok(()) => unreachable!("replicate_cycles бесконечен"),
                    Err(()) => { /* связь потеряна — переподключение */
                    }
                }
            }
            Err(e) => {
                online.store(false, Ordering::Relaxed);
                eprintln!(
                    "timeshift: реплика недоступна ({e}); журнал копится в WAL, повтор через 10 с"
                );
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        }
    }
}

async fn replicate_cycles(wal_dir: &Path, pool: &Pool, online: &Arc<AtomicBool>) -> Result<(), ()> {
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        match replicate_once(wal_dir, pool).await {
            Ok(n) => {
                online.store(true, Ordering::Relaxed);
                if n > 0 {
                    println!("timeshift: дореплицировано событий: {n}");
                }
            }
            Err(e) => {
                online.store(false, Ordering::Relaxed);
                eprintln!("timeshift: репликация отложена: {e}");
                return Err(());
            }
        }
    }
}

/// Прогоняет весь журнал через активный бэкенд; уже применённые события
/// отсекаются по `applied_events`. Возвращает число новых применённых.
pub(crate) async fn replicate_once(wal_dir: &Path, pool: &Pool) -> Result<usize, String> {
    let playback = read_all(wal_dir).map_err(|e| e.to_string())?;
    let mut applied = 0;
    for record in &playback.events {
        applied += match pool {
            Pool::Pg(p) => postgres::replicate_record(p, record).await?,
            Pool::Sq(p) => sqlite::replicate_record(p, record).await?,
        };
    }
    Ok(applied)
}

// --- Общие хелперы маппинга (одни и те же строковые коды в обеих схемах) ----

pub(super) fn err(stage: &'static str) -> impl Fn(sqlx::Error) -> String {
    move |e| format!("{stage}: {e}")
}

pub(super) fn source_code(s: SessionSource) -> &'static str {
    match s {
        SessionSource::Timer => "timer",
        SessionSource::Manual => "manual",
    }
}

pub(super) fn reason_str(r: PauseReason) -> &'static str {
    match r {
        PauseReason::Internal => "internal",
        PauseReason::External => "external",
        PauseReason::Away => "away",
    }
}

pub(super) fn category_str(c: Category) -> &'static str {
    match c {
        Category::Work => "work",
        Category::Personal => "personal",
        Category::Meeting => "meeting",
    }
}

pub(super) fn resolution_str(r: ThoughtResolution) -> &'static str {
    match r {
        ThoughtResolution::Note => "note",
        ThoughtResolution::TaskDraft => "task_draft",
        ThoughtResolution::Dismissed => "dismissed",
    }
}

pub(super) fn break_kind_str(k: BreakKind) -> &'static str {
    match k {
        BreakKind::Short => "short",
        BreakKind::Long => "long",
        BreakKind::Bank => "bank",
    }
}

pub(super) fn break_action_str(a: BreakAction) -> &'static str {
    match a {
        BreakAction::Taken => "taken",
        BreakAction::SkipToShort => "skip_to_short",
        BreakAction::SkipToLong => "skip_to_long",
        BreakAction::SkipPlain => "skip_plain",
    }
}

pub(super) fn plan_section_str(s: PlanSection) -> &'static str {
    match s {
        PlanSection::Work => "work",
        PlanSection::Personal => "personal",
    }
}

#[cfg(test)]
mod tests {
    use super::sqlite;
    use crate::wal::event::{
        GapLogged, PlanBuilt, PlanSection, PlanTask, SessionCompleted, SessionSource,
        SessionStarted,
    };
    use crate::wal::{Wal, read_all};

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tauri::async_runtime::block_on(f)
    }

    /// SQLite-реплика на tempfile: прогон синтетического журнала, числа строк,
    /// второй прогон без дублей. SQLite проверяется без Docker — в отличие от
    /// Postgres-ветки, которую принимают вручную против контейнера.
    #[test]
    fn sqlite_replicates_and_dedups() {
        let tmp = tempfile::tempdir().expect("tmp");
        let wal_dir = tmp.path().join("wal");
        let db_path = tmp.path().join("repl.db");

        let base = chrono::Local::now();
        let day = base.date_naive();
        let session_id = uuid::Uuid::new_v4();
        let task_id = uuid::Uuid::new_v4();
        {
            let mut wal = Wal::open(&wal_dir).expect("wal");
            wal.append_on(
                day,
                base,
                SessionStarted {
                    session_id,
                    planned_min: 25,
                    source: SessionSource::Timer,
                    label: Some("тест".into()),
                    confidential: false,
                    plan_task_id: None,
                },
            )
            .expect("append");
            wal.append_on(
                day,
                base + chrono::Duration::minutes(25),
                SessionCompleted {
                    session_id,
                    actual_min: 25,
                },
            )
            .expect("append");
            wal.append_on(
                day,
                base + chrono::Duration::minutes(30),
                GapLogged {
                    started_at: base,
                    ended_at: base + chrono::Duration::minutes(28),
                    reason: "встреча".into(),
                },
            )
            .expect("append");
            wal.append_on(
                day,
                base + chrono::Duration::minutes(31),
                PlanBuilt {
                    day,
                    tasks: vec![PlanTask {
                        id: task_id,
                        section: PlanSection::Work,
                        title: "Задача".into(),
                        wiki_links: vec!["Проект".into()],
                        marker: None,
                        today_note_line: Some("- [ ] Задача".into()),
                        est_pomodoros: Some(2),
                        done: false,
                        today_note_done: false,
                    }],
                    today_note_snapshot: Some("- [ ] Задача".into()),
                },
            )
            .expect("append");
        }

        block_on(async {
            let pool = sqlite::connect(db_path.to_str().expect("path"))
                .await
                .expect("подключение sqlite");

            let playback = read_all(&wal_dir).expect("журнал");
            assert_eq!(playback.events.len(), 4, "все события записаны");

            let applied = super::replicate_once(&wal_dir, &super::Pool::Sq(pool.clone()))
                .await
                .expect("первый прогон");
            assert_eq!(applied, 4);

            // Сессия завершена: started→completed в одной строке, статусы верные.
            let (status, actual): (String, i32) =
                sqlx::query_as("SELECT status, actual_min FROM focus_sessions")
                    .fetch_one(&pool)
                    .await
                    .expect("сессия");
            assert_eq!(status, "completed");
            assert_eq!(actual, 25);

            let gaps: i32 = sqlx::query_scalar("SELECT COUNT(*) FROM day_gaps")
                .fetch_one(&pool)
                .await
                .expect("gaps");
            assert_eq!(gaps, 1);

            // Задача плана: wiki_links легли JSON-текстом.
            let (links, est): (String, i32) =
                sqlx::query_as("SELECT wiki_links, est_pomodoros FROM day_plan_tasks")
                    .fetch_one(&pool)
                    .await
                    .expect("задача");
            assert_eq!(links, r#"["Проект"]"#);
            assert_eq!(est, 2);

            let events: i32 = sqlx::query_scalar("SELECT COUNT(*) FROM applied_events")
                .fetch_one(&pool)
                .await
                .expect("applied_events");
            assert_eq!(events, 4);

            // Второй прогон того же журнала — без дублей.
            let applied2 = super::replicate_once(&wal_dir, &super::Pool::Sq(pool.clone()))
                .await
                .expect("второй прогон");
            assert_eq!(applied2, 0, "идемпотентность по applied_events");
            let events2: i32 = sqlx::query_scalar("SELECT COUNT(*) FROM applied_events")
                .fetch_one(&pool)
                .await
                .expect("applied_events");
            assert_eq!(events2, 4);
            let sessions: i32 = sqlx::query_scalar("SELECT COUNT(*) FROM focus_sessions")
                .fetch_one(&pool)
                .await
                .expect("сессии без дублей");
            assert_eq!(sessions, 1);
        });
    }
}
