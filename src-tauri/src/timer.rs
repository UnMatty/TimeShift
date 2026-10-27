//! Timer Core — конечный автомат дня (WP-2, ARCHITECTURE §3.1).
//!
//! Единственный мутатор состояния — `fold`: операции (`start_focus`, `pause`, …)
//! только проверяют допустимость и возвращают события; вызывающий код дописывает
//! их в WAL и прогоняет через `fold`. Благодаря этому восстановление после
//! перезапуска — это тот же `fold` поверх всего журнала: убийство процесса
//! посреди фокуса не теряет время (текущий отрезок восстанавливается от
//! штампа последнего события).

#[cfg(test)]
pub mod tests;

use chrono::{DateTime, Local, NaiveDate};
use serde::Serialize;
use uuid::Uuid;

use crate::config::TimerConfig;
use crate::wal::event::*;

// ---------------------------------------------------------------------------
// Модель состояния
// ---------------------------------------------------------------------------

/// Окно разрыва: конец предыдущего этапа, старт нового, причина.
type GapWindow = (DateTime<Local>, DateTime<Local>, String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Running,
    Paused,
    Completed,
    Partial,
    Void,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct UiPause {
    pub started_at: DateTime<Local>,
    pub ended_at: Option<DateTime<Local>>,
    pub reason: Option<PauseReason>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct UiSession {
    pub id: Uuid,
    pub started_at: DateTime<Local>,
    pub ended_at: Option<DateTime<Local>>,
    pub planned_min: i64,
    pub actual_min: Option<i64>,
    pub status: SessionStatus,
    pub category: Option<Category>,
    pub confidential: bool,
    pub note: String,
    pub label: Option<String>,
    pub source: SessionSource,
    pub pauses: Vec<UiPause>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct UiGap {
    pub started_at: DateTime<Local>,
    pub ended_at: DateTime<Local>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct UiThought {
    pub id: Uuid,
    pub created_at: DateTime<Local>,
    pub text: String,
    pub resolved: bool,
    pub resolution: Option<ThoughtResolution>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct UiBreakEvent {
    pub at: DateTime<Local>,
    pub kind: BreakKind,
    pub action: BreakAction,
    pub banked_min: i64,
    pub duration_min: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FocusPhase {
    pub session_id: Uuid,
    pub planned_min: i64,
    pub label: Option<String>,
    pub confidential: bool,
    pub started_at: DateTime<Local>,
    /// Накопленное чистое время фокуса в секундах — без пауз и текущего отрезка.
    pub focus_secs: i64,
    /// Начало текущего рабочего отрезка; `None` — на паузе.
    pub seg_start: Option<DateTime<Local>>,
    pub open_pause_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BreakPhase {
    pub kind: BreakKind,
    pub started_at: DateTime<Local>,
    pub duration_min: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    Idle,
    Focus(FocusPhase),
    Break(BreakPhase),
}

/// Снимок для UI и трея (сериализуется в TS без изменений имён полей).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct Snapshot {
    pub phase: PhaseView,
    pub bank_min: i64,
    pub streak: u32,
    pub day: NaiveDate,
    pub sessions: Vec<UiSession>,
    pub gaps: Vec<UiGap>,
    pub thoughts: Vec<UiThought>,
    pub breaks: Vec<UiBreakEvent>,
    pub proposed_break: Option<ProposedBreak>,
}

/// Фаза в терминах UI: что на экране и сколько осталось.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum PhaseView {
    Idle,
    Focus {
        session_id: Uuid,
        label: Option<String>,
        planned_min: i64,
        elapsed_secs: i64,
        paused: bool,
        confidential: bool,
        pauses: Vec<UiPause>,
    },
    Break {
        break_kind: BreakKind,
        remaining_secs: i64,
        duration_min: i64,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ProposedBreak {
    pub kind: BreakKind,
    pub duration_min: i64,
}

#[derive(Debug, Clone)]
pub struct Timer {
    pub cfg: TimerConfig,
    pub phase: Phase,
    pub short_extra_min: i64,
    pub long_extra_min: i64,
    pub streak: u32,
    pub last_stage_end: Option<DateTime<Local>>,
    pub current_date: NaiveDate,
    pub sessions: Vec<UiSession>,
    pub gaps: Vec<UiGap>,
    pub thoughts: Vec<UiThought>,
    pub breaks: Vec<UiBreakEvent>,
}

impl Timer {
    pub fn new(cfg: TimerConfig) -> Self {
        Timer {
            cfg,
            phase: Phase::Idle,
            short_extra_min: 0,
            long_extra_min: 0,
            streak: 0,
            last_stage_end: None,
            current_date: Local::now().date_naive(),
            sessions: Vec::new(),
            gaps: Vec::new(),
            thoughts: Vec::new(),
            breaks: Vec::new(),
        }
    }

    pub fn bank_min(&self) -> i64 {
        self.short_extra_min + self.long_extra_min
    }

    /// Какой перерыв предлагается после завершённой помидорки (PRD FR-1).
    pub fn proposed_break(&self) -> Option<ProposedBreak> {
        if !matches!(self.phase, Phase::Idle) || self.streak == 0 {
            return None;
        }
        let long = self.streak.is_multiple_of(self.cfg.long_every);
        let (kind, base) = if long {
            (BreakKind::Long, self.cfg.long_break_min)
        } else {
            (BreakKind::Short, self.cfg.short_break_min)
        };
        let extra = match kind {
            BreakKind::Short => self.short_extra_min,
            BreakKind::Long => self.long_extra_min,
        };
        Some(ProposedBreak { kind, duration_min: base + extra })
    }

    /// Снимок для UI/трея. `now` передаётся снаружи — тик и тесты управляют временем.
    pub fn snapshot(&self, now: DateTime<Local>) -> Snapshot {
        let phase = match &self.phase {
            Phase::Idle => PhaseView::Idle,
            Phase::Focus(f) => {
                let elapsed = f.focus_secs
                    + f.seg_start.map(|s| (now - s).num_seconds().max(0)).unwrap_or(0);
                PhaseView::Focus {
                    session_id: f.session_id,
                    label: f.label.clone(),
                    planned_min: f.planned_min,
                    elapsed_secs: elapsed,
                    paused: f.seg_start.is_none(),
                    confidential: f.confidential,
                    pauses: self
                        .current_session_pauses(f.session_id),
                }
            }
            Phase::Break(b) => {
                let remaining = b.duration_min * 60 - (now - b.started_at).num_seconds();
                PhaseView::Break {
                    break_kind: b.kind,
                    remaining_secs: remaining.max(0),
                    duration_min: b.duration_min,
                }
            }
        };
        Snapshot {
            phase,
            bank_min: self.bank_min(),
            streak: self.streak,
            day: self.current_date,
            sessions: self.sessions.clone(),
            gaps: self.gaps.clone(),
            thoughts: self.thoughts.clone(),
            breaks: self.breaks.clone(),
            proposed_break: self.proposed_break(),
        }
    }

    fn current_session_pauses(&self, session_id: Uuid) -> Vec<UiPause> {
        self.sessions
            .iter()
            .find(|s| s.id == session_id)
            .map(|s| s.pauses.clone())
            .unwrap_or_default()
    }

    // -----------------------------------------------------------------------
    // Операции: валидация и конструирование событий. Состояние меняет только fold.
    // -----------------------------------------------------------------------

    /// Старт фокуса (PRD FR-2 — вручную). Если между этапами большой разрыв и
    /// причина не передана — `NeedGap` (FR-4), команда вернёт её в UI.
    pub fn op_start_focus(
        &self,
        now: DateTime<Local>,
        label: Option<String>,
        confidential: bool,
        gap_reason: Option<String>,
    ) -> Result<Vec<Event>, OpError> {
        if !matches!(self.phase, Phase::Idle) {
            return Err(OpError::InvalidState("этап уже идёт".into()));
        }
        let mut events = Vec::new();
        if let Some(gap) = self.detect_gap(now, gap_reason)? {
            events.push(GapLogged { started_at: gap.0, ended_at: gap.1, reason: gap.2 }.into());
        }
        events.push(SessionStarted {
            session_id: Uuid::new_v4(),
            planned_min: self.cfg.focus_min,
            source: SessionSource::Timer,
            label: label.filter(|l| !l.trim().is_empty()),
            confidential,
        }
        .into());
        Ok(events)
    }

    /// Старт предложенного перерыва (короткий/длинный по стрику, с банком).
    pub fn op_start_break(&self, now: DateTime<Local>, gap_reason: Option<String>) -> Result<Vec<Event>, OpError> {
        if !matches!(self.phase, Phase::Idle) {
            return Err(OpError::InvalidState("этап уже идёт".into()));
        }
        let mut events = Vec::new();
        if let Some(gap) = self.detect_gap(now, gap_reason)? {
            events.push(GapLogged { started_at: gap.0, ended_at: gap.1, reason: gap.2 }.into());
        }
        let proposed = self
            .proposed_break()
            .unwrap_or(ProposedBreak { kind: BreakKind::Short, duration_min: self.cfg.short_break_min });
        let banked_after = self.bank_min()
            - match proposed.kind {
                BreakKind::Short => self.short_extra_min,
                BreakKind::Long => self.long_extra_min,
            };
        events.push(
            BreakEvent {
                kind: proposed.kind,
                action: BreakAction::Taken,
                banked_min: banked_after,
                duration_min: proposed.duration_min,
            }
            .into(),
        );
        Ok(events)
    }

    /// Пропуск перерыва с выбором, куда прибавить его минуты (PRD FR-5).
    pub fn op_skip_break(&self, action: BreakAction) -> Result<Vec<Event>, OpError> {
        if !matches!(self.phase, Phase::Idle) {
            return Err(OpError::InvalidState("перерыв пропускается, когда этапа нет".into()));
        }
        let proposed = self
            .proposed_break()
            .ok_or_else(|| OpError::InvalidState("нет предложенного перерыва".into()))?;
        let offered_base = match proposed.kind {
            BreakKind::Short => self.cfg.short_break_min,
            BreakKind::Long => self.cfg.long_break_min,
        };
        let banked_after = match action {
            BreakAction::SkipToShort => self.bank_min() + offered_base,
            BreakAction::SkipToLong => self.bank_min() + offered_base,
            BreakAction::SkipPlain => self.bank_min(),
            BreakAction::Taken => return Err(OpError::InvalidState("taken — не пропуск".into())),
        };
        Ok(vec![BreakEvent {
            kind: proposed.kind,
            action,
            banked_min: banked_after,
            duration_min: 0,
        }
        .into()])
    }

    pub fn op_pause_focus(&self) -> Result<Vec<Event>, OpError> {
        let Phase::Focus(f) = &self.phase else {
            return Err(OpError::InvalidState("пауза — только во время фокуса".into()));
        };
        if f.seg_start.is_none() {
            return Err(OpError::InvalidState("фокус уже на паузе".into()));
        }
        Ok(vec![SessionPaused { session_id: f.session_id, pause_id: Uuid::new_v4() }.into()])
    }

    /// Возобновление с причиной паузы (PRD FR-3): два события — ответ и само продолжение.
    pub fn op_resume_focus(&self, reason: PauseReason) -> Result<Vec<Event>, OpError> {
        let Phase::Focus(f) = &self.phase else {
            return Err(OpError::InvalidState("фокус не идёт".into()));
        };
        let Some(pause_id) = f.open_pause_id else {
            return Err(OpError::InvalidState("фокус не на паузе".into()));
        };
        Ok(vec![
            PauseReasonEvent { session_id: f.session_id, pause_id, reason }.into(),
            SessionResumed { session_id: f.session_id }.into(),
        ])
    }

    /// Завершить помидорку сейчас (досрочно = partial, но засчитывается).
    pub fn op_complete_focus(&self, now: DateTime<Local>) -> Result<Vec<Event>, OpError> {
        let Phase::Focus(f) = &self.phase else {
            return Err(OpError::InvalidState("фокус не идёт".into()));
        };
        let actual_min = round_up_min(self.focus_total_secs(f, now));
        Ok(vec![SessionCompleted { session_id: f.session_id, actual_min }.into()])
    }

    /// Аннулирование (PRD FR-6): фиксируется отдельно, в стрик не идёт.
    pub fn op_void_focus(&self, reason: Option<String>) -> Result<Vec<Event>, OpError> {
        let Phase::Focus(f) = &self.phase else {
            return Err(OpError::InvalidState("фокус не идёт".into()));
        };
        Ok(vec![SessionVoided { session_id: f.session_id, reason }.into()])
    }

    /// Разорвать перерыв досрочно. Состояние локальное — без события.
    pub fn op_end_break(&mut self, now: DateTime<Local>) -> Result<(), OpError> {
        if !matches!(self.phase, Phase::Break(_)) {
            return Err(OpError::InvalidState("перерыв не идёт".into()));
        }
        self.phase = Phase::Idle;
        self.last_stage_end = Some(now);
        Ok(())
    }

    pub fn op_park_thought(&self, text: String) -> Result<Vec<Event>, OpError> {
        let text = text.trim().to_string();
        if text.is_empty() {
            return Err(OpError::InvalidState("пустая мысль".into()));
        }
        Ok(vec![ThoughtParked { thought_id: Uuid::new_v4(), text }.into()])
    }

    pub fn op_resolve_thought(&self, thought_id: Uuid, resolution: ThoughtResolution) -> Result<Vec<Event>, OpError> {
        let thought = self
            .thoughts
            .iter()
            .find(|t| t.id == thought_id)
            .ok_or(OpError::UnknownThought)?;
        if thought.resolved {
            return Err(OpError::InvalidState("мысль уже разобрана".into()));
        }
        Ok(vec![ThoughtResolved { thought_id, resolution }.into()])
    }

    pub fn op_set_note(&self, session_id: Uuid, note: String) -> Result<Vec<Event>, OpError> {
        if !self.sessions.iter().any(|s| s.id == session_id) {
            return Err(OpError::UnknownSession);
        }
        Ok(vec![NoteEdited { session_id, note }.into()])
    }

    pub fn op_set_category(&self, session_id: Uuid, category: Category) -> Result<Vec<Event>, OpError> {
        if !self.sessions.iter().any(|s| s.id == session_id) {
            return Err(OpError::UnknownSession);
        }
        Ok(vec![CategorySet { session_id, category }.into()])
    }

    /// Ручная до-логировка «работал без приложения» (PRD FR-7).
    pub fn op_add_manual_session(
        &self,
        started_at: DateTime<Local>,
        actual_min: i64,
        label: Option<String>,
        note: Option<String>,
        category: Option<Category>,
    ) -> Result<Vec<Event>, OpError> {
        if actual_min <= 0 {
            return Err(OpError::InvalidState("длительность должна быть больше нуля".into()));
        }
        let ended_at = started_at + chrono::Duration::minutes(actual_min);
        Ok(vec![ManualSessionAdded {
            session_id: Uuid::new_v4(),
            started_at,
            ended_at,
            actual_min,
            label: label.filter(|l| !l.trim().is_empty()),
            note: note.filter(|n| !n.trim().is_empty()),
            category,
        }
        .into()])
    }

    /// «Завершить день» (v0.1-часть, PRD FR-5): банк сгорает, стрик обнуляется.
    pub fn op_close_day(&self) -> Result<Vec<Event>, OpError> {
        if !matches!(self.phase, Phase::Idle) {
            return Err(OpError::InvalidState("сначала заверши текущий этап".into()));
        }
        let bank = self.bank_min();
        Ok(vec![DayClosed { burned_min: bank, banked_min_at_close: bank }.into()])
    }

    /// Смена суток (тик в полночь): сгоревший банк фиксируется событием.
    pub fn op_rollover_day(&self) -> Option<Vec<Event>> {
        if self.bank_min() == 0 && self.streak == 0 {
            return None;
        }
        let bank = self.bank_min();
        Some(vec![DayClosed { burned_min: bank, banked_min_at_close: bank }.into()])
    }

    // -----------------------------------------------------------------------
    // fold — единственный мутатор состояния
    // -----------------------------------------------------------------------

    pub fn fold(&mut self, record: &WalRecord) {
        use Event as E;
        let ts = record.ts;
        match record.event.clone() {
            E::SessionStarted(e) => {
                self.sessions.push(UiSession {
                    id: e.session_id,
                    started_at: ts,
                    ended_at: None,
                    planned_min: e.planned_min,
                    actual_min: None,
                    status: SessionStatus::Running,
                    category: None,
                    confidential: false,
                    note: String::new(),
                    label: e.label,
                    source: e.source,
                    pauses: Vec::new(),
                });
                if e.source == SessionSource::Timer {
                    self.phase = Phase::Focus(FocusPhase {
                        session_id: e.session_id,
                        planned_min: e.planned_min,
                        label: self.sessions.last().and_then(|s| s.label.clone()),
                        confidential: e.confidential,
                        started_at: ts,
                        focus_secs: 0,
                        seg_start: Some(ts),
                        open_pause_id: None,
                    });
                }
                if let Some(s) = session_mut(&mut self.sessions, e.session_id) {
                    s.confidential = e.confidential;
                }
            }
            E::SessionPaused(e) => {
                if let Phase::Focus(f) = &mut self.phase
                    && f.session_id == e.session_id {
                        f.focus_secs += seg_secs(f, ts);
                        f.seg_start = None;
                        f.open_pause_id = Some(e.pause_id);
                    }
                if let Some(s) = session_mut(&mut self.sessions, e.session_id) {
                    s.status = SessionStatus::Paused;
                    s.pauses.push(UiPause { started_at: ts, ended_at: None, reason: None });
                }
            }
            E::PauseReason(e) => {
                if let Some(s) = session_mut(&mut self.sessions, e.session_id)
                    && let Some(open) = s.pauses.iter_mut().rev().find(|p| p.ended_at.is_none()) {
                        open.reason = Some(e.reason);
                    }
            }
            E::SessionResumed(e) => {
                if let Phase::Focus(f) = &mut self.phase
                    && f.session_id == e.session_id {
                        f.seg_start = Some(ts);
                        f.open_pause_id = None;
                    }
                if let Some(s) = session_mut(&mut self.sessions, e.session_id) {
                    s.status = SessionStatus::Running;
                    if let Some(open) = s.pauses.iter_mut().rev().find(|p| p.ended_at.is_none()) {
                        open.ended_at = Some(ts);
                    }
                }
            }
            E::SessionCompleted(e) => {
                if let Phase::Focus(f) = &mut self.phase
                    && f.session_id == e.session_id {
                        f.focus_secs += seg_secs(f, ts);
                        f.seg_start = None;
                    }
                if let Some(s) = session_mut(&mut self.sessions, e.session_id) {
                    s.actual_min = Some(e.actual_min);
                    s.ended_at = Some(ts);
                    let planned = s.planned_min;
                    s.status = if e.actual_min >= planned {
                        SessionStatus::Completed
                    } else {
                        SessionStatus::Partial
                    };
                }
                self.phase = Phase::Idle;
                self.last_stage_end = Some(ts);
                self.streak += 1;
            }
            E::SessionVoided(e) => {
                if let Phase::Focus(f) = &mut self.phase
                    && f.session_id == e.session_id {
                        f.focus_secs += seg_secs(f, ts);
                        f.seg_start = None;
                    }
                if let Some(s) = session_mut(&mut self.sessions, e.session_id) {
                    s.ended_at = Some(ts);
                    s.status = SessionStatus::Void;
                }
                self.phase = Phase::Idle;
                self.last_stage_end = Some(ts);
            }
            E::GapLogged(e) => self.gaps.push(UiGap {
                started_at: e.started_at,
                ended_at: e.ended_at,
                reason: e.reason,
            }),
            E::NoteEdited(e) => {
                if let Some(s) = session_mut(&mut self.sessions, e.session_id) {
                    s.note = e.note;
                }
            }
            E::CategorySet(e) => {
                if let Some(s) = session_mut(&mut self.sessions, e.session_id) {
                    s.category = Some(e.category);
                }
            }
            E::ThoughtParked(e) => self.thoughts.push(UiThought {
                id: e.thought_id,
                created_at: ts,
                text: e.text,
                resolved: false,
                resolution: None,
            }),
            E::ThoughtResolved(e) => {
                if let Some(t) = self.thoughts.iter_mut().find(|t| t.id == e.thought_id) {
                    t.resolved = true;
                    t.resolution = Some(e.resolution);
                }
            }
            E::BreakEvent(e) => {
                let offered_base = match e.kind {
                    BreakKind::Short => self.cfg.short_break_min,
                    BreakKind::Long => self.cfg.long_break_min,
                };
                match e.action {
                    BreakAction::Taken => {
                        // Длительность уже включает банк этого вида — банк потреблён.
                        match e.kind {
                            BreakKind::Short => self.short_extra_min = 0,
                            BreakKind::Long => self.long_extra_min = 0,
                        }
                        self.phase = Phase::Break(BreakPhase {
                            kind: e.kind,
                            started_at: ts,
                            duration_min: if e.duration_min > 0 { e.duration_min } else { offered_base },
                        });
                    }
                    BreakAction::SkipToShort => self.short_extra_min += offered_base,
                    BreakAction::SkipToLong => self.long_extra_min += offered_base,
                    BreakAction::SkipPlain => {}
                }
                self.breaks.push(UiBreakEvent {
                    at: ts,
                    kind: e.kind,
                    action: e.action,
                    banked_min: e.banked_min,
                    duration_min: e.duration_min,
                });
            }
            E::ManualSessionAdded(e) => {
                self.sessions.push(UiSession {
                    id: e.session_id,
                    started_at: e.started_at,
                    ended_at: Some(e.ended_at),
                    planned_min: e.actual_min,
                    actual_min: Some(e.actual_min),
                    status: SessionStatus::Completed,
                    category: e.category,
                    confidential: false,
                    note: e.note.unwrap_or_default(),
                    label: e.label,
                    source: SessionSource::Manual,
                    pauses: Vec::new(),
                });
            }
            E::DayClosed(e) => {
                self.short_extra_min = 0;
                self.long_extra_min = 0;
                self.streak = 0;
                let _ = e;
            }
            E::VaultSyncStatus(_) => {}
        }
        if ts.date_naive() > self.current_date {
            self.current_date = ts.date_naive();
        }
    }

    // -----------------------------------------------------------------------
    // Вспомогательное
    // -----------------------------------------------------------------------

    /// Разрыв между этапами (PRD FR-4): `Ok(None)` — разрыва нет,
    /// `Ok(Some)` — есть и причина передана, `Err(NeedGap)` — есть, причины нет.
    fn detect_gap(
        &self,
        now: DateTime<Local>,
        gap_reason: Option<String>,
    ) -> Result<Option<GapWindow>, OpError> {
        let Some(end) = self.last_stage_end else { return Ok(None) };
        let minutes = (now - end).num_minutes();
        if minutes < self.cfg.gap_threshold_min {
            return Ok(None);
        }
        let Some(reason) = gap_reason.filter(|r| !r.trim().is_empty()) else {
            return Err(OpError::NeedGap { minutes });
        };
        Ok(Some((end, now, reason.trim().to_string())))
    }

    fn focus_total_secs(&self, f: &FocusPhase, now: DateTime<Local>) -> i64 {
        f.focus_secs + f.seg_start.map(|s| (now - s).num_seconds().max(0)).unwrap_or(0)
    }

    /// Естественное завершение по тику. Возвращает события (может быть пусто).
    pub fn tick(&mut self, now: DateTime<Local>) -> Vec<Event> {
        match self.phase.clone() {
            Phase::Focus(f) if f.seg_start.is_some() => {
                if self.focus_total_secs(&f, now) >= f.planned_min * 60 {
                    let actual_min = round_up_min(self.focus_total_secs(&f, now));
                    return vec![SessionCompleted { session_id: f.session_id, actual_min }.into()];
                }
            }
            Phase::Break(b)
                if (now - b.started_at).num_seconds() >= b.duration_min * 60 => {
                    // Конец перерыва — локальное состояние; на_timeline остаётся
                    // восстановимым из BreakEvent(started_at + duration).
                    self.phase = Phase::Idle;
                    self.last_stage_end = Some(now);
                }
            _ => {}
        }
        Vec::new()
    }

    /// Смена суток: событие сгорания банка или просто сдвиг даты.
    pub fn tick_day_rollover(&mut self, now: DateTime<Local>) -> Vec<Event> {
        let today = now.date_naive();
        if today == self.current_date {
            return Vec::new();
        }
        if let Some(events) = self.op_rollover_day() {
            return events;
        }
        self.current_date = today;
        Vec::new()
    }
}

/// Ошибки операций — сериализуются в UI как структурированный ответ.
#[derive(Debug, thiserror::Error)]
pub enum OpError {
    /// Большой разрыв: нужен ответ «где был» (FR-4).
    #[error("разрыв {minutes} мин — нужна причина")]
    NeedGap { minutes: i64 },
    #[error("операция недопустима: {0}")]
    InvalidState(String),
    #[error("сессия не найдена")]
    UnknownSession,
    #[error("мысль не найдена")]
    UnknownThought,
}

fn session_mut(sessions: &mut [UiSession], id: Uuid) -> Option<&mut UiSession> {
    sessions.iter_mut().find(|s| s.id == id)
}

/// Секунды текущего рабочего отрезка до момента `ts` (если не на паузе).
fn seg_secs(f: &FocusPhase, ts: DateTime<Local>) -> i64 {
    f.seg_start.map(|s| (ts - s).num_seconds().max(0)).unwrap_or(0)
}

/// Чистые минуты фокуса, округлённые вверх, минимум 1.
fn round_up_min(secs: i64) -> i64 {
    ((secs + 59) / 60).max(1)
}
