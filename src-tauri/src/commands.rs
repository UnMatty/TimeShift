//! Слой Tauri-команд (WP-3): валидация операции → события в WAL → fold →
//! снимок в UI. Один и тот же `run_op` используют и команды окна, и меню трея.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use chrono::{Local, NaiveTime};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use uuid::Uuid;

use crate::config::{Config, TimerConfig};
use crate::timer::{OpError, Snapshot, Timer};
use crate::wal::event::{WalRecord, *};
use crate::wal::Wal;

/// Общее состояние приложения.
pub struct AppState {
    pub timer: Mutex<Timer>,
    pub wal: Mutex<Wal>,
    pub config: Mutex<Config>,
    pub db_online: Arc<AtomicBool>,
    /// Хендл трея для обновления заголовка (ставится в setup).
    pub tray: OnceLock<tauri::tray::TrayIcon>,
}

impl AppState {
    pub fn new(timer: Timer, wal: Wal, config: Config) -> Self {
        Self {
            timer: Mutex::new(timer),
            wal: Mutex::new(wal),
            config: Mutex::new(config),
            db_online: Arc::new(AtomicBool::new(false)),
            tray: OnceLock::new(),
        }
    }

    /// Клон флага «БД онлайн» для фоновых задач.
    pub fn db_online_flag(&self) -> Arc<AtomicBool> {
        self.db_online.clone()
    }

    pub fn db_online(&self) -> bool {
        self.db_online.load(Ordering::Relaxed)
    }
}

/// Ошибка команды в UI: структурированная (нужен ответ на вопрос о разрыве)
/// или текстовая.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CmdError {
    /// Большой разрыв между этапами — UI должен спросить «где был?» (FR-4).
    GapNeeded { minutes: i64 },
    Message { text: String },
}

impl From<OpError> for CmdError {
    fn from(e: OpError) -> Self {
        match e {
            OpError::NeedGap { minutes } => CmdError::GapNeeded { minutes },
            other => CmdError::Message { text: other.to_string() },
        }
    }
}

/// Исполняет операцию: события → WAL → fold → снимок. Общая для окна и трея.
pub fn run_op<F>(app: &AppHandle, op: F) -> Result<Snapshot, CmdError>
where
    F: FnOnce(&Timer) -> Result<Vec<Event>, OpError>,
{
    let state = app.state::<AppState>();
    let events = op(&state.timer.lock().expect("таймер"))?;

    let mut records = Vec::with_capacity(events.len());
    {
        let mut wal = state.wal.lock().expect("журнал");
        for event in events {
            records.push(wal.append(event).map_err(|e| CmdError::Message { text: e.to_string() })?);
        }
    }
    let snapshot = {
        let mut timer = state.timer.lock().expect("таймер");
        for record in &records {
            timer.fold(record);
        }
        timer.snapshot(Local::now())
    };

    notify_side_effects(app, &records);
    app.emit("tick", &snapshot).ok();
    update_tray(app, &snapshot);
    Ok(snapshot)
}

/// Уведомления, привязанные к событиям (PRD FR-27…29).
/// Публичная: тик вызывает её же для своих событий.
pub fn notify_side_effects(app: &AppHandle, records: &[WalRecord]) {
    for record in records {
        match &record.event {
            Event::SessionCompleted(e) => {
                let (planned, next_break) = {
                    let state = app.state::<AppState>();
                    let timer = state.timer.lock().expect("таймер");
                    let planned = timer
                        .sessions
                        .iter()
                        .find(|s| s.id == e.session_id)
                        .map(|s| s.planned_min)
                        .unwrap_or(25);
                    (planned, timer.proposed_break())
                };
                let break_hint = match next_break {
                    Some(p) if p.kind == BreakKind::Long => format!("длинный перерыв {} мин", p.duration_min),
                    Some(p) => format!("перерыв {} мин", p.duration_min),
                    None => "перерыв".into(),
                };
                notify(
                    app,
                    "🍅 Помидорка завершена",
                    &format!("{planned} минут фокуса. Пора на {break_hint}."),
                );
            }
            Event::BreakEvent(e) if e.action == BreakAction::Taken && e.kind == BreakKind::Long => {
                // Долгий перерыв заслуживает настойчивого напоминания.
                notify(app, "☕ Длинный перерыв", &format!("{} минут. Отойди от экрана.", e.duration_min));
            }
            _ => {}
        }
    }
}

/// Мягкое уведомление об окончании перерыва (FR-28) — из тика и end_break.
pub fn notify_break_over(app: &AppHandle) {
    notify(app, "Перерыв окончен", "Возвращаемся. Старт следующей помидорки — за тобой.");
}

/// Напоминание о накопленном банке отдыха при старте фокуса (FR-29).
pub fn maybe_notify_bank(app: &AppHandle) {
    let state = app.state::<AppState>();
    let bank = state.timer.lock().expect("таймер").bank_min();
    if bank >= 15 {
        notify(app, "У тебя в банке отдыха лежит минут", &format!("{bank} минут нерастраченного отдыха — потрать их на перерыве."));
    }
}

fn notify(app: &AppHandle, title: &str, body: &str) {
    use tauri_plugin_notification::NotificationExt;
    app.notification()
        .builder()
        .title(title)
        .body(body)
        .show()
        .ok();
}

/// Обновляет заголовок иконки в меню-баре (ARCHITECTURE §7).
pub fn update_tray(app: &AppHandle, snap: &Snapshot) {
    let state = app.state::<AppState>();
    let Some(tray) = state.tray.get() else { return };
    let title = match &snap.phase {
        crate::timer::PhaseView::Idle => "◦".into(),
        crate::timer::PhaseView::Focus { planned_min, elapsed_secs, paused, .. } => {
            let remaining = (planned_min * 60 - elapsed_secs).max(0);
            let mark = if *paused { "⏸" } else { "🍅" };
            format!("{mark} {}:{:02}", remaining / 60, remaining % 60)
        }
        crate::timer::PhaseView::Break { break_kind, remaining_secs, .. } => {
            let mark = if *break_kind == BreakKind::Long { "☕" } else { "💬" };
            format!("{mark} {}:{:02}", remaining_secs / 60, remaining_secs % 60)
        }
    };
    tray.set_title(Some(title.as_str())).ok();
}

// ---------------------------------------------------------------------------
// Команды
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn get_snapshot(app: AppHandle) -> Snapshot {
    let state = app.state::<AppState>();
    state.timer.lock().expect("таймер").snapshot(Local::now())
}

#[tauri::command]
pub fn db_status(app: AppHandle) -> bool {
    app.state::<AppState>().db_online()
}

#[tauri::command]
pub fn start_focus(
    app: AppHandle,
    label: Option<String>,
    confidential: bool,
    gap_reason: Option<String>,
) -> Result<Snapshot, CmdError> {
    let snap = run_op(&app, |t| t.op_start_focus(Local::now(), label, confidential, gap_reason))?;
    maybe_notify_bank(&app);
    Ok(snap)
}

#[tauri::command]
pub fn pause_focus(app: AppHandle) -> Result<Snapshot, CmdError> {
    run_op(&app, |t| t.op_pause_focus())
}

#[tauri::command]
pub fn resume_focus(app: AppHandle, reason: PauseReason) -> Result<Snapshot, CmdError> {
    run_op(&app, |t| t.op_resume_focus(reason))
}

#[tauri::command]
pub fn complete_focus(app: AppHandle) -> Result<Snapshot, CmdError> {
    run_op(&app, |t| t.op_complete_focus(Local::now()))
}

#[tauri::command]
pub fn void_session(app: AppHandle, reason: Option<String>) -> Result<Snapshot, CmdError> {
    run_op(&app, |t| t.op_void_focus(reason))
}

#[tauri::command]
pub fn start_break(app: AppHandle, gap_reason: Option<String>) -> Result<Snapshot, CmdError> {
    run_op(&app, |t| t.op_start_break(Local::now(), gap_reason))
}

#[tauri::command]
pub fn skip_break(app: AppHandle, action: BreakAction) -> Result<Snapshot, CmdError> {
    run_op(&app, |t| t.op_skip_break(action))
}

#[tauri::command]
pub fn end_break(app: AppHandle) -> Result<Snapshot, CmdError> {
    {
        let state = app.state::<AppState>();
        let mut timer = state.timer.lock().expect("таймер");
        timer.op_end_break(Local::now()).map_err(CmdError::from)?;
    }
    notify_break_over(&app);
    let snap = get_snapshot(app.clone());
    app.emit("tick", &snap).ok();
    update_tray(&app, &snap);
    Ok(snap)
}

#[tauri::command]
pub fn park_thought(app: AppHandle, text: String) -> Result<Snapshot, CmdError> {
    run_op(&app, |t| t.op_park_thought(text))
}

#[tauri::command]
pub fn resolve_thought(
    app: AppHandle,
    thought_id: Uuid,
    resolution: ThoughtResolution,
) -> Result<Snapshot, CmdError> {
    run_op(&app, |t| t.op_resolve_thought(thought_id, resolution))
}

#[tauri::command]
pub fn set_note(app: AppHandle, session_id: Uuid, note: String) -> Result<Snapshot, CmdError> {
    run_op(&app, |t| t.op_set_note(session_id, note))
}

#[tauri::command]
pub fn set_category(app: AppHandle, session_id: Uuid, category: Category) -> Result<Snapshot, CmdError> {
    run_op(&app, |t| t.op_set_category(session_id, category))
}

/// Ручная до-логировка: время передаётся как «ЧЧ:ММ» сегодняшнего дня.
#[tauri::command]
pub fn add_manual_session(
    app: AppHandle,
    started_at: String,
    minutes: i64,
    label: Option<String>,
    note: Option<String>,
    category: Option<Category>,
) -> Result<Snapshot, CmdError> {
    use chrono::TimeZone as _;
    let time = NaiveTime::parse_from_str(&started_at, "%H:%M")
        .map_err(|_| CmdError::Message { text: "время в формате ЧЧ:ММ".into() })?;
    let naive = Local::now().date_naive().and_time(time);
    let start = Local
        .from_local_datetime(&naive)
        .single()
        .ok_or_else(|| CmdError::Message { text: "некорректное локальное время".into() })?;
    run_op(&app, |t| t.op_add_manual_session(start, minutes, label, note, category))
}

#[tauri::command]
pub fn close_day(app: AppHandle) -> Result<Snapshot, CmdError> {
    run_op(&app, |t| t.op_close_day())
}

// ---------------------------------------------------------------------------
// Настройки
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct SettingsDto {
    pub timer: TimerConfig,
    pub autostart: bool,
    pub db_online: bool,
}

#[tauri::command]
pub fn get_settings(app: AppHandle) -> SettingsDto {
    let state = app.state::<AppState>();
    let config = state.config.lock().expect("конфиг");
    SettingsDto {
        timer: config.timer,
        autostart: config.app.autostart,
        db_online: state.db_online(),
    }
}

#[tauri::command]
pub fn save_settings(
    app: AppHandle,
    focus_min: i64,
    short_break_min: i64,
    long_break_min: i64,
    long_every: u32,
    gap_threshold_min: i64,
    autostart: bool,
) -> Result<SettingsDto, CmdError> {
    let state = app.state::<AppState>();
    {
        let mut config = state.config.lock().expect("конфиг");
        config.timer = TimerConfig {
            focus_min: focus_min.max(1),
            short_break_min: short_break_min.max(1),
            long_break_min: long_break_min.max(1),
            long_every: long_every.max(2),
            gap_threshold_min: gap_threshold_min.max(1),
        };
        config.app.autostart = autostart;
        config.save().map_err(|e| CmdError::Message { text: format!("не удалось сохранить конфиг: {e}") })?;
        state.timer.lock().expect("таймер").cfg = config.timer;
    }

    #[cfg(target_os = "macos")]
    {
        use tauri_plugin_autostart::ManagerExt as _;
        if autostart {
            app.autolaunch().enable().ok();
        } else {
            app.autolaunch().disable().ok();
        }
    }

    Ok(get_settings(app))
}
