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
use crate::vault::client::{VaultClient, VaultError};
use crate::vault::today::{parse_today, TodayTask};
use crate::vault::TODAY_NOTE_PATH;
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

impl From<VaultError> for CmdError {
    fn from(e: VaultError) -> Self {
        CmdError::Message { text: e.to_string() }
    }
}

/// Клиент хранилища из текущего конфига. Не настроен — внятная ошибка.
pub fn vault_client(app: &AppHandle) -> Result<VaultClient, CmdError> {
    let state = app.state::<AppState>();
    let cfg = state.config.lock().expect("конфиг");
    let url = cfg
        .vault
        .api_url
        .as_deref()
        .ok_or_else(|| CmdError::Message { text: "хранилище не настроено: нет api_url в ~/.timeshift/config.toml".into() })?;
    let key = cfg
        .vault
        .api_key
        .as_deref()
        .ok_or_else(|| CmdError::Message { text: "хранилище не настроено: нет api_key в ~/.timeshift/config.toml".into() })?;
    VaultClient::new(url, key).map_err(|e| CmdError::Message { text: e })
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
            Event::BreakEvent(e)
                if e.action == BreakAction::Taken
                    && e.kind == BreakKind::Long
                    && e.spend_min.is_none() =>
            {
                // Долгий перерыв заслуживает настойчивого напоминания
                // (запись «разрыв = отдых» — не про отдых у экрана).
                notify(app, "☕ Длинный перерыв", &format!("{} минут. Отойди от экрана.", e.duration_min));
            }
            Event::DayClosed(e) => {
                notify(
                    app,
                    "🌙 День закрыт",
                    &format!("Банк отдыха сгорел: {} мин. Завтра — новый день.", e.burned_min),
                );
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

pub(crate) fn notify(app: &AppHandle, title: &str, body: &str) {
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
            let mark = match break_kind {
                BreakKind::Long => "☕",
                BreakKind::Bank => "🛌",
                BreakKind::Short => "💬",
            };
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
    gap_as_rest: bool,
    category: Option<Category>,
    plan_task_id: Option<Uuid>,
) -> Result<Snapshot, CmdError> {
    // Старт по задаче плана: подпись берём из задачи, категорию наследуем из
    // секции (FR-13), если пользователь не выбрал свою.
    let resolved = plan_task_id.map(|id| {
        let state = app.state::<AppState>();
        let timer = state.timer.lock().expect("таймер");
        timer
            .plan
            .as_ref()
            .and_then(|p| p.tasks.iter().find(|t| t.id == id))
            .map(|t| (t.title.clone(), t.section))
    });
    let (task_label, task_category) = resolved.flatten().unwrap_or_else(|| (String::new(), PlanSection::Work));
    let label = match (&label, plan_task_id.is_some()) {
        (Some(l), _) => Some(l.clone()),
        (None, true) => Some(task_label),
        (None, false) => None,
    };
    let category = category.or_else(|| {
        plan_task_id.map(|_| match task_category {
            PlanSection::Work => Category::Work,
            PlanSection::Personal => Category::Personal,
        })
    });

    let snap = run_op(&app, |t| {
        t.op_start_focus(Local::now(), label, confidential, gap_reason, gap_as_rest, category, plan_task_id)
    })?;
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
pub fn start_break(
    app: AppHandle,
    gap_reason: Option<String>,
    gap_as_rest: bool,
) -> Result<Snapshot, CmdError> {
    run_op(&app, |t| t.op_start_break(Local::now(), gap_reason, gap_as_rest))
}

/// «Отдохнуть весь накопленный банк» (v0.1.1).
#[tauri::command]
pub fn start_bank_break(
    app: AppHandle,
    gap_reason: Option<String>,
    gap_as_rest: bool,
) -> Result<Snapshot, CmdError> {
    run_op(&app, |t| t.op_start_bank_break(Local::now(), gap_reason, gap_as_rest))
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
// План дня (v0.2, PRD FR-14…FR-18)
// ---------------------------------------------------------------------------

/// Импорт плана из «☀️ Сегодня»: свежее чтение → парсер → событие PlanBuilt.
/// Ноль чекбоксов — не тихий пустой план, а внятная ошибка (ARCH §9).
#[tauri::command]
pub async fn build_plan_from_today(app: AppHandle) -> Result<Snapshot, CmdError> {
    let client = vault_client(&app)?;
    let raw = client.read_note(TODAY_NOTE_PATH).await?;
    let note = parse_today(&raw);
    if note.is_empty() {
        return Err(CmdError::Message {
            text: "в «☀️ Сегодня» не нашлось ни одного чекбокса — заметка пустая или формат изменился".into(),
        });
    }
    let day = {
        let state = app.state::<AppState>();
        state.timer.lock().expect("таймер").current_date
    };
    let tasks: Vec<PlanTask> = note.tasks.into_iter().map(today_task_into_plan).collect();
    run_op(&app, |t| t.op_build_plan(day, tasks, Some(raw)))
}

fn today_task_into_plan(t: TodayTask) -> PlanTask {
    PlanTask {
        id: Uuid::new_v4(),
        section: t.section,
        title: t.title,
        wiki_links: t.wiki_links,
        marker: t.marker,
        today_note_line: Some(t.raw_line),
        est_pomodoros: None,
        done: t.done,
        today_note_done: t.done,
    }
}

#[tauri::command]
pub fn add_plan_task(
    app: AppHandle,
    section: PlanSection,
    title: String,
    est_pomodoros: Option<u32>,
) -> Result<Snapshot, CmdError> {
    run_op(&app, |t| t.op_add_plan_task(section, title, est_pomodoros))
}

#[tauri::command]
pub fn remove_plan_task(app: AppHandle, task_id: Uuid) -> Result<Snapshot, CmdError> {
    run_op(&app, |t| t.op_remove_plan_task(task_id))
}

/// Раскладка по секциям и/или оценка; null/отсутствие поля = не менять.
#[tauri::command]
pub fn edit_plan_task(
    app: AppHandle,
    task_id: Uuid,
    section: Option<PlanSection>,
    est_pomodoros: Option<u32>,
) -> Result<Snapshot, CmdError> {
    run_op(&app, |t| t.op_edit_plan_task(task_id, section, est_pomodoros))
}

#[tauri::command]
pub fn set_plan_task_done(app: AppHandle, task_id: Uuid, done: bool) -> Result<Snapshot, CmdError> {
    run_op(&app, |t| t.op_set_plan_task_done(task_id, done))
}

#[tauri::command]
pub fn add_meeting(
    app: AppHandle,
    title: String,
    starts_at: Option<String>,
    duration_min: Option<i64>,
) -> Result<Snapshot, CmdError> {
    let starts_at = match starts_at.as_deref() {
        None | Some("") => None,
        Some(hhmm) => {
            use chrono::TimeZone as _;
            let time = NaiveTime::parse_from_str(hhmm, "%H:%M")
                .map_err(|_| CmdError::Message { text: "время встречи в формате ЧЧ:ММ".into() })?;
            let naive = Local::now().date_naive().and_time(time);
            Some(
                Local.from_local_datetime(&naive).single().ok_or_else(|| CmdError::Message {
                    text: "некорректное локальное время встречи".into(),
                })?,
            )
        }
    };
    run_op(&app, |t| t.op_add_meeting(title, starts_at, duration_min))
}

#[tauri::command]
pub fn set_meeting_happened(app: AppHandle, meeting_id: Uuid, happened: bool) -> Result<Snapshot, CmdError> {
    run_op(&app, |t| t.op_set_meeting_happened(meeting_id, happened))
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
