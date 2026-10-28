//! Репликатор WAL → Postgres (WP-4, ARCHITECTURE §3.3).
//!
//! Правило идемпотентности: событие применяется в той же транзакции, что и
//! вставка в `applied_events` — повторный прогон журнала не создаёт дублей.
//! Docker остановлен или БД недоступна — цикл молча ретраит; в UI статус
//! отдаётся через `db_online`.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Transaction};
use uuid::Uuid;

use crate::wal::event::*;
use crate::wal::read_all;

/// Подключается и применяет миграции (`../migrations` относительно src-tauri).
pub async fn connect(url: &str) -> Result<PgPool, String> {
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .acquire_timeout(Duration::from_secs(3))
        .connect(url)
        .await
        .map_err(|e| format!("подключение к БД: {e}"))?;
    sqlx::migrate!("../migrations")
        .run(&pool)
        .await
        .map_err(|e| format!("миграции: {e}"))?;
    Ok(pool)
}

/// Вечный цикл: подключение → репликация → при потере связи переподключение.
pub async fn replicator_loop(url: String, wal_dir: std::path::PathBuf, online: Arc<AtomicBool>) {
    loop {
        match connect(&url).await {
            Ok(pool) => {
                online.store(true, Ordering::Relaxed);
                match replicate_cycles(&wal_dir, &pool, &online).await {
                    Ok(()) => unreachable!("replicate_cycles бесконечен"),
                    Err(()) => { /* связь потеряна — переподключение */ }
                }
            }
            Err(e) => {
                online.store(false, Ordering::Relaxed);
                eprintln!("timeshift: БД недоступна ({e}); журнал копится в WAL, повтор через 10 с");
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        }
    }
}

async fn replicate_cycles(wal_dir: &Path, pool: &PgPool, online: &Arc<AtomicBool>) -> Result<(), ()> {
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

/// Прогоняет весь журнал через БД; уже применённые события отсекаются
/// по `applied_events`. Возвращает число новых применённых.
async fn replicate_once(wal_dir: &Path, pool: &PgPool) -> Result<usize, String> {
    let playback = read_all(wal_dir).map_err(|e| e.to_string())?;
    let mut applied = 0;
    for record in &playback.events {
        let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
        let inserted = sqlx::query(
            "INSERT INTO applied_events (event_id) VALUES ($1) ON CONFLICT (event_id) DO NOTHING RETURNING event_id",
        )
        .bind(record.id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
        if inserted.is_some() {
            apply_event(&mut tx, record).await?;
            applied += 1;
        }
        tx.commit().await.map_err(|e| e.to_string())?;
    }
    Ok(applied)
}

/// Маппинг события на схему (ARCHITECTURE §4). Строковые коды статусов/категорий —
/// те же, что в комментариях миграции.
async fn apply_event(tx: &mut Transaction<'static, sqlx::Postgres>, record: &WalRecord) -> Result<(), String> {
    let ts = record.ts;
    match record.event.clone() {
        Event::SessionStarted(e) => {
            sqlx::query(
                "INSERT INTO focus_sessions (id, started_at, planned_min, status, confidential, note, source, label, plan_task_id)
                 VALUES ($1, $2, $3, 'running', $4, '', $5, $6, $7)",
            )
            .bind(e.session_id)
            .bind(ts)
            .bind(e.planned_min as i32)
            .bind(e.confidential)
            .bind(source_str(e.source))
            .bind(e.label)
            .bind(e.plan_task_id)
            .execute(&mut **tx)
            .await
            .map_err(err("session_started"))?;
        }
        Event::SessionPaused(e) => {
            sqlx::query("UPDATE focus_sessions SET status = 'paused' WHERE id = $1")
                .bind(e.session_id)
                .execute(&mut **tx)
                .await
                .map_err(err("session_paused/status"))?;
            sqlx::query("INSERT INTO session_pauses (id, session_id, started_at) VALUES ($1, $2, $3)")
                .bind(e.pause_id)
                .bind(e.session_id)
                .bind(ts)
                .execute(&mut **tx)
                .await
                .map_err(err("session_pauses/insert"))?;
        }
        Event::PauseReason(e) => {
            sqlx::query("UPDATE session_pauses SET reason = $2 WHERE id = $1")
                .bind(e.pause_id)
                .bind(reason_str(e.reason))
                .execute(&mut **tx)
                .await
                .map_err(err("pause_reason"))?;
        }
        Event::SessionResumed(e) => {
            sqlx::query("UPDATE focus_sessions SET status = 'running' WHERE id = $1")
                .bind(e.session_id)
                .execute(&mut **tx)
                .await
                .map_err(err("session_resumed/status"))?;
            sqlx::query("UPDATE session_pauses SET ended_at = $2 WHERE session_id = $1 AND ended_at IS NULL")
                .bind(e.session_id)
                .bind(ts)
                .execute(&mut **tx)
                .await
                .map_err(err("session_resumed/pause"))?;
        }
        Event::SessionCompleted(e) => {
            sqlx::query(
                "UPDATE focus_sessions
                 SET ended_at = $2, actual_min = $3,
                     status = CASE WHEN $3 >= planned_min THEN 'completed' ELSE 'partial' END
                 WHERE id = $1",
            )
            .bind(e.session_id)
            .bind(ts)
            .bind(e.actual_min as i32)
            .execute(&mut **tx)
            .await
            .map_err(err("session_completed"))?;
        }
        Event::SessionVoided(e) => {
            sqlx::query("UPDATE focus_sessions SET ended_at = $2, status = 'void', void_reason = $3 WHERE id = $1")
                .bind(e.session_id)
                .bind(ts)
                .bind(e.reason)
                .execute(&mut **tx)
                .await
                .map_err(err("session_voided"))?;
        }
        Event::GapLogged(e) => {
            sqlx::query("INSERT INTO day_gaps (id, day, started_at, ended_at, reason) VALUES ($1, $2, $3, $4, $5)")
                .bind(Uuid::new_v4())
                .bind(e.started_at.date_naive())
                .bind(e.started_at)
                .bind(e.ended_at)
                .bind(e.reason)
                .execute(&mut **tx)
                .await
                .map_err(err("gap_logged"))?;
        }
        Event::NoteEdited(e) => {
            sqlx::query("UPDATE focus_sessions SET note = $2 WHERE id = $1")
                .bind(e.session_id)
                .bind(e.note)
                .execute(&mut **tx)
                .await
                .map_err(err("note_edited"))?;
        }
        Event::CategorySet(e) => {
            sqlx::query("UPDATE focus_sessions SET category = $2 WHERE id = $1")
                .bind(e.session_id)
                .bind(category_str(e.category))
                .execute(&mut **tx)
                .await
                .map_err(err("category_set"))?;
        }
        Event::ThoughtParked(e) => {
            sqlx::query("INSERT INTO parked_thoughts (id, created_at, text) VALUES ($1, $2, $3)")
                .bind(e.thought_id)
                .bind(ts)
                .bind(e.text)
                .execute(&mut **tx)
                .await
                .map_err(err("thought_parked"))?;
        }
        Event::ThoughtResolved(e) => {
            sqlx::query("UPDATE parked_thoughts SET resolved_at = $2, resolution = $3 WHERE id = $1")
                .bind(e.thought_id)
                .bind(ts)
                .bind(resolution_str(e.resolution))
                .execute(&mut **tx)
                .await
                .map_err(err("thought_resolved"))?;
        }
        Event::BreakEvent(e) => {
            sqlx::query(
                "INSERT INTO break_events (id, at, kind, action, banked_min) VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(record.id)
            .bind(ts)
            .bind(break_kind_str(e.kind))
            .bind(break_action_str(e.action))
            .bind(e.banked_min as i32)
            .execute(&mut **tx)
            .await
            .map_err(err("break_event"))?;
        }
        Event::ManualSessionAdded(e) => {
            sqlx::query(
                "INSERT INTO focus_sessions (id, started_at, ended_at, planned_min, actual_min, status, category, confidential, note, source, label)
                 VALUES ($1, $2, $3, $4, $4, 'completed', $5, false, $6, 'manual', $7)",
            )
            .bind(e.session_id)
            .bind(e.started_at)
            .bind(e.ended_at)
            .bind(e.actual_min as i32)
            .bind(e.category.map(category_str))
            .bind(e.note)
            .bind(e.label)
            .execute(&mut **tx)
            .await
            .map_err(err("manual_session_added"))?;
        }
        Event::DayClosed(e) => {
            sqlx::query(
                "INSERT INTO break_events (id, at, kind, action, banked_min, burned_min) VALUES ($1, $2, 'day_close', 'burn', $3, $4)",
            )
            .bind(record.id)
            .bind(ts)
            .bind(e.banked_min_at_close as i32)
            .bind(e.burned_min as i32)
            .execute(&mut **tx)
            .await
            .map_err(err("day_closed"))?;
        }
        Event::PlanBuilt(e) => {
            sqlx::query(
                "INSERT INTO day_plans (id, day, today_note_snapshot) VALUES ($1, $2, $3)
                 ON CONFLICT (day) DO UPDATE SET today_note_snapshot = EXCLUDED.today_note_snapshot",
            )
            .bind(Uuid::new_v4())
            .bind(e.day)
            .bind(e.today_note_snapshot)
            .execute(&mut **tx)
            .await
            .map_err(err("plan_built/plan"))?;
            // Пересборка заменяет задачи целиком (встречи в fold хранятся).
            sqlx::query("DELETE FROM day_plan_tasks WHERE plan_id = (SELECT id FROM day_plans WHERE day = $1)")
                .bind(e.day)
                .execute(&mut **tx)
                .await
                .map_err(err("plan_built/clear"))?;
            for (position, task) in e.tasks.iter().enumerate() {
                let plan_id: Uuid = sqlx::query_scalar("SELECT id FROM day_plans WHERE day = $1")
                    .bind(e.day)
                    .fetch_one(&mut **tx)
                    .await
                    .map_err(err("plan_built/plan_id"))?;
                sqlx::query(
                    "INSERT INTO day_plan_tasks (id, plan_id, section, title, wiki_links, markers, today_note_line, est_pomodoros, position, done, today_note_done)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
                )
                .bind(task.id)
                .bind(plan_id)
                .bind(plan_section_str(task.section))
                .bind(&task.title)
                .bind(&task.wiki_links)
                .bind(task.marker.as_deref())
                .bind(task.today_note_line.as_deref())
                .bind(task.est_pomodoros.map(|e| e as i32))
                .bind(position as i32)
                .bind(task.done)
                .bind(task.today_note_done)
                .execute(&mut **tx)
                .await
                .map_err(err("plan_built/task"))?;
            }
        }
        Event::PlanTaskAdded(e) => {
            sqlx::query(
                "INSERT INTO day_plan_tasks (id, plan_id, section, title, wiki_links, markers, today_note_line, est_pomodoros, position, done, today_note_done)
                 VALUES ($1, (SELECT id FROM day_plans WHERE day = $2), $3, $4, $5, $6, $7, $8,
                         (SELECT COALESCE(MAX(position), 0) + 1 FROM day_plan_tasks WHERE plan_id = (SELECT id FROM day_plans WHERE day = $2)), $9, $10)",
            )
            .bind(e.task.id)
            .bind(ts.date_naive())
            .bind(plan_section_str(e.task.section))
            .bind(&e.task.title)
            .bind(&e.task.wiki_links)
            .bind(e.task.marker.as_deref())
            .bind(e.task.today_note_line.as_deref())
            .bind(e.task.est_pomodoros.map(|x| x as i32))
            .bind(e.task.done)
            .bind(e.task.today_note_done)
            .execute(&mut **tx)
            .await
            .map_err(err("plan_task_added"))?;
        }
        Event::PlanTaskRemoved(e) => {
            // Сессии, привязанные к задаче, отвязываем — FK не должен рвать репликацию.
            sqlx::query("UPDATE focus_sessions SET plan_task_id = NULL WHERE plan_task_id = $1")
                .bind(e.task_id)
                .execute(&mut **tx)
                .await
                .map_err(err("plan_task_removed/detach"))?;
            sqlx::query("DELETE FROM day_plan_tasks WHERE id = $1")
                .bind(e.task_id)
                .execute(&mut **tx)
                .await
                .map_err(err("plan_task_removed"))?;
        }
        Event::PlanTaskEdited(e) => {
            // Поле-Option обновляется, только если в событии задано (None = не менялось).
            sqlx::query(
                "UPDATE day_plan_tasks SET
                   section = COALESCE($2, section),
                   est_pomodoros = COALESCE($3, est_pomodoros)
                 WHERE id = $1",
            )
            .bind(e.task_id)
            .bind(e.section.map(plan_section_str))
            .bind(e.est_pomodoros.map(|x| x as i32))
            .execute(&mut **tx)
            .await
            .map_err(err("plan_task_edited"))?;
        }
        Event::PlanTaskCompleted(e) => {
            sqlx::query("UPDATE day_plan_tasks SET done = $2 WHERE id = $1")
                .bind(e.task_id)
                .bind(e.done)
                .execute(&mut **tx)
                .await
                .map_err(err("plan_task_completed"))?;
        }
        Event::PlanTaskSynced(e) => {
            if e.conflict.is_none() {
                sqlx::query("UPDATE day_plan_tasks SET today_note_done = $2 WHERE id = $1")
                    .bind(e.task_id)
                    .bind(e.today_note_done)
                    .execute(&mut **tx)
                    .await
                    .map_err(err("plan_task_synced"))?;
            }
        }
        Event::MeetingAdded(e) => {
            sqlx::query(
                "INSERT INTO plan_meetings (id, plan_id, title, starts_at, duration_min, happened)
                 VALUES ($1, (SELECT id FROM day_plans WHERE day = $2), $3, $4, $5, $6)",
            )
            .bind(e.meeting.id)
            .bind(ts.date_naive())
            .bind(&e.meeting.title)
            .bind(e.meeting.starts_at)
            .bind(e.meeting.duration_min.map(|m| m as i32))
            .bind(e.meeting.happened)
            .execute(&mut **tx)
            .await
            .map_err(err("meeting_added"))?;
        }
        Event::MeetingHappened(e) => {
            sqlx::query("UPDATE plan_meetings SET happened = $2 WHERE id = $1")
                .bind(e.meeting_id)
                .bind(e.happened)
                .execute(&mut **tx)
                .await
                .map_err(err("meeting_happened"))?;
        }
        Event::VaultSyncStatus(_) => {}
    }
    Ok(())
}

fn err(stage: &'static str) -> impl Fn(sqlx::Error) -> String {
    move |e| format!("{stage}: {e}")
}

fn source_str(s: SessionSource) -> &'static str {
    match s {
        SessionSource::Timer => "timer",
        SessionSource::Manual => "manual",
    }
}

fn reason_str(r: PauseReason) -> &'static str {
    match r {
        PauseReason::Internal => "internal",
        PauseReason::External => "external",
        PauseReason::Away => "away",
    }
}

fn category_str(c: Category) -> &'static str {
    match c {
        Category::Work => "work",
        Category::Personal => "personal",
        Category::Meeting => "meeting",
    }
}

fn resolution_str(r: ThoughtResolution) -> &'static str {
    match r {
        ThoughtResolution::Note => "note",
        ThoughtResolution::TaskDraft => "task_draft",
        ThoughtResolution::Dismissed => "dismissed",
    }
}

fn break_kind_str(k: BreakKind) -> &'static str {
    match k {
        BreakKind::Short => "short",
        BreakKind::Long => "long",
        BreakKind::Bank => "bank",
    }
}

fn break_action_str(a: BreakAction) -> &'static str {
    match a {
        BreakAction::Taken => "taken",
        BreakAction::SkipToShort => "skip_to_short",
        BreakAction::SkipToLong => "skip_to_long",
        BreakAction::SkipPlain => "skip_plain",
    }
}

fn plan_section_str(s: PlanSection) -> &'static str {
    match s {
        PlanSection::Work => "work",
        PlanSection::Personal => "personal",
    }
}
