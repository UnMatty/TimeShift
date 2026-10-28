//! Типы событий WAL — контракт с Replicator (ARCHITECTURE §3.2).
//!
//! Каждая запись журнала — одна строка JSONL:
//! `{"id": <uuid>, "ts": <RFC3339 с локальным смещением>, "type": "...", "payload": {...}}`
//! Формат менять нельзя без согласования с Replicator (WP-4) и будущей миграцией WAL.

use chrono::{DateTime, Local, NaiveDate};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Причина паузы (PRD FR-3): спрос при возобновлении.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseReason {
    /// Сам отвлёкся.
    Internal,
    /// Отвлекли.
    External,
    /// Отошёл.
    Away,
}

/// Категория сессии (PRD FR-12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Work,
    Personal,
    Meeting,
}

/// Секция плана дня (PRD FR-14): работа / личное. Встречи — отдельный список
/// (`PlanMeeting`), не задача с оценкой.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanSection {
    Work,
    Personal,
}

/// Задача плана дня: импортирована из «☀️ Сегодня» или добавлена вручную.
/// `today_note_line` — точная строка заметки для обратной точечной замены
/// чекбокса (привязка по тексту, не по номеру — ARCH §9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanTask {
    pub id: Uuid,
    pub section: PlanSection,
    pub title: String,
    #[serde(default)]
    pub wiki_links: Vec<String>,
    #[serde(default)]
    pub marker: Option<String>,
    #[serde(default)]
    pub today_note_line: Option<String>,
    /// Оценка в помидорках (план, FR-16).
    #[serde(default)]
    pub est_pomodoros: Option<u32>,
    /// Отмечена выполненной в приложении.
    #[serde(default)]
    pub done: bool,
    /// Чекбокс в «☀️ Сегодня» проставлен синком (может отставать от `done`).
    #[serde(default)]
    pub today_note_done: bool,
}

/// Ручная запись о встрече дня (PRD FR-17).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanMeeting {
    pub id: Uuid,
    pub title: String,
    #[serde(default)]
    pub starts_at: Option<DateTime<Local>>,
    #[serde(default)]
    pub duration_min: Option<i64>,
    /// Вечерняя отметка: состоялась (иначе перенесена/отменена).
    #[serde(default)]
    pub happened: bool,
}

/// Откуда сессия: таймер или ручная до-логировка (PRD FR-7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionSource {
    Timer,
    Manual,
}

/// Вид перерыва (PRD FR-1). `Bank` — «отдохнуть весь накопленный банк» (v0.1.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BreakKind {
    Short,
    Long,
    Bank,
}

/// Действие с перерывом (PRD FR-5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BreakAction {
    Taken,
    SkipToShort,
    SkipToLong,
    SkipPlain,
}

/// Разбор парковой мысли (PRD FR-11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThoughtResolution {
    Note,
    TaskDraft,
    Dismissed,
}

/// Старт фокус-сессии из таймера.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionStarted {
    pub session_id: Uuid,
    pub planned_min: i64,
    pub source: SessionSource,
    /// Свободная подпись задачи (для задач плана дублирует их заголовок).
    pub label: Option<String>,
    /// Тексты такой сессии не уходят в LLM-обзор (PRD FR-26, важно с v0.3).
    #[serde(default)]
    pub confidential: bool,
    /// Задача плана дня, под которую стартовала помидорка (v0.2, FR-16).
    #[serde(default)]
    pub plan_task_id: Option<Uuid>,
}

/// Пауза фокуса. Причина приходит отдельным событием `pause_reason`
/// при возобновлении (в этот момент `session_pauses.ended_at` закрывается).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionPaused {
    pub session_id: Uuid,
    pub pause_id: Uuid,
}

/// Ответ «почему пауза».
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PauseReasonEvent {
    pub session_id: Uuid,
    pub pause_id: Uuid,
    pub reason: PauseReason,
}

/// Возобновление фокуса после паузы.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionResumed {
    pub session_id: Uuid,
}

/// Завершённая помидорка.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionCompleted {
    pub session_id: Uuid,
    /// Чистое время фокуса без пауз.
    pub actual_min: i64,
}

/// Аннулирование помидорки (PRD FR-6) — фиксируется отдельно от завершённой.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionVoided {
    pub session_id: Uuid,
    pub reason: Option<String>,
}

/// Ретро-логирование разрыва между этапами (PRD FR-4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GapLogged {
    pub started_at: DateTime<Local>,
    pub ended_at: DateTime<Local>,
    /// обед | встреча | потерял_ритм | иное — свободный текст (ARCH §4, day_gaps.reason).
    pub reason: String,
}

/// Заметка «что сделано» (PRD FR-8); правки перезаписывают последнюю версию.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NoteEdited {
    pub session_id: Uuid,
    pub note: String,
}

/// Классификация сессии постфактум (PRD FR-12).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CategorySet {
    pub session_id: Uuid,
    pub category: Category,
}

/// Мысль на парковке (PRD FR-10).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThoughtParked {
    pub thought_id: Uuid,
    pub text: String,
}

/// Разбор парковой мысли (PRD FR-11).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThoughtResolved {
    pub thought_id: Uuid,
    pub resolution: ThoughtResolution,
}

/// Событие перерыва: взят или пропущен, состояние банка после события (ARCH §4).
/// Для взятого перерыва `duration_min` — фактическая длительность (база + банк):
/// по ней проигрывание восстанавливает идущий перерыв после перезапуска.
/// `spend_min` (v0.1.1, «разрыв = отдых») — сколько минут списать из банка,
/// если это не весь банк данного вида; `None` — прежнее поведение. Поле
/// опционально, поэтому старые записи журнала читаются без миграции.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BreakEvent {
    pub kind: BreakKind,
    pub action: BreakAction,
    pub banked_min: i64,
    #[serde(default)]
    pub duration_min: i64,
    #[serde(default)]
    pub spend_min: Option<i64>,
}

/// Ручная до-логировка сессии «работал без приложения» (PRD FR-7).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManualSessionAdded {
    pub session_id: Uuid,
    pub started_at: DateTime<Local>,
    pub ended_at: DateTime<Local>,
    pub actual_min: i64,
    #[serde(default)]
    pub label: Option<String>,
    pub note: Option<String>,
    pub category: Option<Category>,
}

/// «Завершить день»: фиксация сгоревшего банка отдыха (PRD FR-5, FR-23 — без LLM до v0.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DayClosed {
    /// Сколько минут банка сгорело.
    pub burned_min: i64,
    /// Полное состояние банка на момент закрытия (для аудита).
    pub banked_min_at_close: i64,
}

// --- План дня (v0.2, PRD FR-14…FR-18) --------------------------------------

/// Сборка/пересборка плана дня (импорт из «☀️ Сегодня» заменяет план целиком).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanBuilt {
    pub day: NaiveDate,
    pub tasks: Vec<PlanTask>,
    /// Сырой текст «☀️ Сегодня» на момент сборки (аудит конфликтов, day_plans.today_note_snapshot).
    #[serde(default)]
    pub today_note_snapshot: Option<String>,
}

/// Ручное добавление задачи в план.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanTaskAdded {
    pub task: PlanTask,
}

/// Удаление задачи из плана (саму строку «Сегодня» не трогаем — ARCH §5.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanTaskRemoved {
    pub task_id: Uuid,
}

/// Правка задачи: раскладка по секциям и/или оценка в помидорках (FR-14, FR-16).
/// `None` поля не меняются (двойные Option в JSON не переживают раундтрип WAL).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanTaskEdited {
    pub task_id: Uuid,
    #[serde(default)]
    pub section: Option<PlanSection>,
    #[serde(default)]
    pub est_pomodoros: Option<u32>,
}

/// Отметка задачи в приложении (галочка в UI). Чекбокс в «Сегодня» проставляет
/// vault-синк отдельным событием `PlanTaskSynced` (FR-18).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanTaskCompleted {
    pub task_id: Uuid,
    pub done: bool,
}

/// Факт синхронизации чекбокса с «☀️ Сегодня» (ok — совпало).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanTaskSynced {
    pub task_id: Uuid,
    pub today_note_done: bool,
    /// Почему не получилось (конфликт: строка пересобрана агентом) — для аудита.
    #[serde(default)]
    pub conflict: Option<String>,
}

/// Ручная запись о встрече (FR-17).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeetingAdded {
    pub meeting: PlanMeeting,
}

/// Вечерняя отметка встречи: состоялась / перенесена.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeetingHappened {
    pub meeting_id: Uuid,
    pub happened: bool,
}

/// Статус синхронизации с внешними системами (vault/БД); в v0.1 пишет Replicator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VaultSyncStatus {
    pub ok: bool,
    pub detail: Option<String>,
}

/// Все события журнала. `#[non_exhaustive]`: новые варианты добавляются в конец,
/// читатели старых версий не должны матчиться exhaustively.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum Event {
    SessionStarted(SessionStarted),
    SessionPaused(SessionPaused),
    PauseReason(PauseReasonEvent),
    SessionResumed(SessionResumed),
    SessionCompleted(SessionCompleted),
    SessionVoided(SessionVoided),
    GapLogged(GapLogged),
    NoteEdited(NoteEdited),
    CategorySet(CategorySet),
    ThoughtParked(ThoughtParked),
    ThoughtResolved(ThoughtResolved),
    BreakEvent(BreakEvent),
    ManualSessionAdded(ManualSessionAdded),
    DayClosed(DayClosed),
    PlanBuilt(PlanBuilt),
    PlanTaskAdded(PlanTaskAdded),
    PlanTaskRemoved(PlanTaskRemoved),
    PlanTaskEdited(PlanTaskEdited),
    PlanTaskCompleted(PlanTaskCompleted),
    PlanTaskSynced(PlanTaskSynced),
    MeetingAdded(MeetingAdded),
    MeetingHappened(MeetingHappened),
    VaultSyncStatus(VaultSyncStatus),
}

/// Запись журнала. `Event` разворачивается в ключи `type`/`payload` верхнего уровня.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WalRecord {
    pub id: Uuid,
    pub ts: DateTime<Local>,
    #[serde(flatten)]
    pub event: Event,
}

impl WalRecord {
    /// Штампует событие текущим моментом и новым uuid.
    pub fn now(event: impl Into<Event>) -> Self {
        Self {
            id: Uuid::new_v4(),
            ts: Local::now(),
            event: event.into(),
        }
    }
}

impl From<SessionStarted> for Event {
    fn from(v: SessionStarted) -> Self {
        Event::SessionStarted(v)
    }
}
impl From<SessionPaused> for Event {
    fn from(v: SessionPaused) -> Self {
        Event::SessionPaused(v)
    }
}
impl From<PauseReasonEvent> for Event {
    fn from(v: PauseReasonEvent) -> Self {
        Event::PauseReason(v)
    }
}
impl From<SessionResumed> for Event {
    fn from(v: SessionResumed) -> Self {
        Event::SessionResumed(v)
    }
}
impl From<SessionCompleted> for Event {
    fn from(v: SessionCompleted) -> Self {
        Event::SessionCompleted(v)
    }
}
impl From<SessionVoided> for Event {
    fn from(v: SessionVoided) -> Self {
        Event::SessionVoided(v)
    }
}
impl From<GapLogged> for Event {
    fn from(v: GapLogged) -> Self {
        Event::GapLogged(v)
    }
}
impl From<NoteEdited> for Event {
    fn from(v: NoteEdited) -> Self {
        Event::NoteEdited(v)
    }
}
impl From<CategorySet> for Event {
    fn from(v: CategorySet) -> Self {
        Event::CategorySet(v)
    }
}
impl From<ThoughtParked> for Event {
    fn from(v: ThoughtParked) -> Self {
        Event::ThoughtParked(v)
    }
}
impl From<ThoughtResolved> for Event {
    fn from(v: ThoughtResolved) -> Self {
        Event::ThoughtResolved(v)
    }
}
impl From<BreakEvent> for Event {
    fn from(v: BreakEvent) -> Self {
        Event::BreakEvent(v)
    }
}
impl From<ManualSessionAdded> for Event {
    fn from(v: ManualSessionAdded) -> Self {
        Event::ManualSessionAdded(v)
    }
}
impl From<DayClosed> for Event {
    fn from(v: DayClosed) -> Self {
        Event::DayClosed(v)
    }
}
impl From<PlanBuilt> for Event {
    fn from(v: PlanBuilt) -> Self {
        Event::PlanBuilt(v)
    }
}
impl From<PlanTaskAdded> for Event {
    fn from(v: PlanTaskAdded) -> Self {
        Event::PlanTaskAdded(v)
    }
}
impl From<PlanTaskRemoved> for Event {
    fn from(v: PlanTaskRemoved) -> Self {
        Event::PlanTaskRemoved(v)
    }
}
impl From<PlanTaskEdited> for Event {
    fn from(v: PlanTaskEdited) -> Self {
        Event::PlanTaskEdited(v)
    }
}
impl From<PlanTaskCompleted> for Event {
    fn from(v: PlanTaskCompleted) -> Self {
        Event::PlanTaskCompleted(v)
    }
}
impl From<PlanTaskSynced> for Event {
    fn from(v: PlanTaskSynced) -> Self {
        Event::PlanTaskSynced(v)
    }
}
impl From<MeetingAdded> for Event {
    fn from(v: MeetingAdded) -> Self {
        Event::MeetingAdded(v)
    }
}
impl From<MeetingHappened> for Event {
    fn from(v: MeetingHappened) -> Self {
        Event::MeetingHappened(v)
    }
}
impl From<VaultSyncStatus> for Event {
    fn from(v: VaultSyncStatus) -> Self {
        Event::VaultSyncStatus(v)
    }
}
