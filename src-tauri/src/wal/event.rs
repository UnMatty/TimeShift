//! Типы событий WAL — контракт с Replicator (ARCHITECTURE §3.2).
//!
//! Каждая запись журнала — одна строка JSONL:
//! `{"id": <uuid>, "ts": <RFC3339 с локальным смещением>, "type": "...", "payload": {...}}`
//! Формат менять нельзя без согласования с Replicator (WP-4) и будущей миграцией WAL.

use chrono::{DateTime, Local};
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

/// Откуда сессия: таймер или ручная до-логировка (PRD FR-7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionSource {
    Timer,
    Manual,
}

/// Вид перерыва (PRD FR-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BreakKind {
    Short,
    Long,
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
    /// Свободная подпись задачи (привязка к плану дня появится в v0.2).
    pub label: Option<String>,
    /// Тексты такой сессии не уходят в LLM-обзор (PRD FR-26, важно с v0.3).
    #[serde(default)]
    pub confidential: bool,
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BreakEvent {
    pub kind: BreakKind,
    pub action: BreakAction,
    pub banked_min: i64,
    #[serde(default)]
    pub duration_min: i64,
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
impl From<VaultSyncStatus> for Event {
    fn from(v: VaultSyncStatus) -> Self {
        Event::VaultSyncStatus(v)
    }
}
