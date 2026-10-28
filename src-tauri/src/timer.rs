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

/// План дня (v0.2, PRD §4.5): задачи из «☀️ Сегодня» + ручные, встречи вручную.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DayPlan {
    pub day: NaiveDate,
    pub tasks: Vec<PlanTask>,
    pub meetings: Vec<PlanMeeting>,
}

/// Задача плана в терминах UI: состояние + факт (завершённые помидорки по ней).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct UiPlanTask {
    pub id: Uuid,
    pub section: PlanSection,
    pub title: String,
    pub wiki_links: Vec<String>,
    pub marker: Option<String>,
    pub est_pomodoros: Option<u32>,
    pub done: bool,
    /// Чекбокс в «☀️ Сегодня» уже проставлен синком.
    pub today_note_done: bool,
    /// Задача пришла из «Сегодня» (есть строка для обратной синхронизации).
    pub from_today_note: bool,
    /// Факт: завершённые (полностью или частично) помидорки по задаче.
    pub fact_pomodoros: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct UiPlanMeeting {
    pub id: Uuid,
    pub title: String,
    pub starts_at: Option<DateTime<Local>>,
    pub duration_min: Option<i64>,
    pub happened: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct UiPlan {
    pub day: NaiveDate,
    pub tasks: Vec<UiPlanTask>,
    pub meetings: Vec<UiPlanMeeting>,
}

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
    /// Задача плана дня, под которую шла помидорка (v0.2).
    pub plan_task_id: Option<Uuid>,
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
    pub day_closed: bool,
    /// Сколько минут банка сгорело при закрытии дня (для фокус-лога и UI).
    pub burned_bank_min: i64,
    pub plan: Option<UiPlan>,
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
    /// День закрыт кнопкой «Завершить день» (до полуночи повторно не закрывается).
    pub day_closed: bool,
    /// Сколько банка сгорело при закрытии дня (для фокус-лога).
    pub burned_bank_min: i64,
    /// План дня (v0.2): задачи из «☀️ Сегодня» + ручные, встречи.
    pub plan: Option<DayPlan>,
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
            day_closed: false,
            burned_bank_min: 0,
            plan: None,
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
            // «Весь банк» не предлагается сам — только по явной команде.
            BreakKind::Bank => 0,
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
            day_closed: self.day_closed,
            burned_bank_min: self.burned_bank_min,
            plan: self.plan_view(),
            sessions: self.sessions.clone(),
            gaps: self.gaps.clone(),
            thoughts: self.thoughts.clone(),
            breaks: self.breaks.clone(),
            proposed_break: self.proposed_break(),
        }
    }

    /// План дня для UI: задачи с фактом по завершённым сессиям (FR-16).
    fn plan_view(&self) -> Option<UiPlan> {
        let plan = self.plan.as_ref()?;
        let fact = |task_id: Uuid| {
            self.sessions
                .iter()
                .filter(|s| {
                    s.plan_task_id == Some(task_id)
                        && matches!(s.status, SessionStatus::Completed | SessionStatus::Partial)
                })
                .count() as u32
        };
        Some(UiPlan {
            day: plan.day,
            tasks: plan
                .tasks
                .iter()
                .map(|t| UiPlanTask {
                    id: t.id,
                    section: t.section,
                    title: t.title.clone(),
                    wiki_links: t.wiki_links.clone(),
                    marker: t.marker.clone(),
                    est_pomodoros: t.est_pomodoros,
                    done: t.done,
                    today_note_done: t.today_note_done,
                    from_today_note: t.today_note_line.is_some(),
                    fact_pomodoros: fact(t.id),
                })
                .collect(),
            meetings: plan
                .meetings
                .iter()
                .map(|m| UiPlanMeeting {
                    id: m.id,
                    title: m.title.clone(),
                    starts_at: m.starts_at,
                    duration_min: m.duration_min,
                    happened: m.happened,
                })
                .collect(),
        })
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
    /// `gap_as_rest` — ответ «это был отдых»: минуты разрыва списываются из банка,
    /// остаток сверх банка логируется обычным разрывом. `category` — тип фокуса,
    /// выбранный до старта (v0.1.1). `plan_task_id` — задача плана дня (v0.2).
    /// Аргументов осознанно много — это плоский вход одной операции FSM.
    #[allow(clippy::too_many_arguments)]
    pub fn op_start_focus(
        &self,
        now: DateTime<Local>,
        label: Option<String>,
        confidential: bool,
        gap_reason: Option<String>,
        gap_as_rest: bool,
        category: Option<Category>,
        plan_task_id: Option<Uuid>,
    ) -> Result<Vec<Event>, OpError> {
        if !matches!(self.phase, Phase::Idle) {
            return Err(OpError::InvalidState("этап уже идёт".into()));
        }
        if let Some(id) = plan_task_id {
            let known = self
                .plan
                .as_ref()
                .is_some_and(|p| p.tasks.iter().any(|t| t.id == id));
            if !known {
                return Err(OpError::InvalidState("задача плана не найдена".into()));
            }
        }
        let mut events = Vec::new();
        if let Some(gap) = self.detect_gap(now, gap_reason)? {
            events.extend(self.gap_events(gap, gap_as_rest));
        }
        let session_id = Uuid::new_v4();
        events.push(SessionStarted {
            session_id,
            planned_min: self.cfg.focus_min,
            source: SessionSource::Timer,
            label: label.filter(|l| !l.trim().is_empty()),
            confidential,
            plan_task_id,
        }
        .into());
        if let Some(category) = category {
            events.push(CategorySet { session_id, category }.into());
        }
        Ok(events)
    }

    /// Старт предложенного перерыва (короткий/длинный по стрику, с банком).
    pub fn op_start_break(
        &self,
        now: DateTime<Local>,
        gap_reason: Option<String>,
        gap_as_rest: bool,
    ) -> Result<Vec<Event>, OpError> {
        if !matches!(self.phase, Phase::Idle) {
            return Err(OpError::InvalidState("этап уже идёт".into()));
        }
        let mut events = Vec::new();
        if let Some(gap) = self.detect_gap(now, gap_reason)? {
            events.extend(self.gap_events(gap, gap_as_rest));
        }
        let proposed = self
            .proposed_break()
            .unwrap_or(ProposedBreak { kind: BreakKind::Short, duration_min: self.cfg.short_break_min });
        let banked_after = self.bank_min()
            - match proposed.kind {
                BreakKind::Short => self.short_extra_min,
                BreakKind::Long => self.long_extra_min,
                BreakKind::Bank => self.bank_min(),
            };
        events.push(
            BreakEvent {
                kind: proposed.kind,
                action: BreakAction::Taken,
                banked_min: banked_after,
                duration_min: proposed.duration_min,
                spend_min: None,
            }
            .into(),
        );
        Ok(events)
    }

    /// «Отдохнуть весь банк» (v0.1.1): перерыв длительностью во весь накопленный
    /// отдых; банк обнуляется.
    pub fn op_start_bank_break(
        &self,
        now: DateTime<Local>,
        gap_reason: Option<String>,
        gap_as_rest: bool,
    ) -> Result<Vec<Event>, OpError> {
        if !matches!(self.phase, Phase::Idle) {
            return Err(OpError::InvalidState("этап уже идёт".into()));
        }
        let bank = self.bank_min();
        if bank <= 0 {
            return Err(OpError::InvalidState("банк отдыха пуст".into()));
        }
        let mut events = Vec::new();
        if let Some(gap) = self.detect_gap(now, gap_reason)? {
            events.extend(self.gap_events(gap, gap_as_rest));
        }
        events.push(
            BreakEvent {
                kind: BreakKind::Bank,
                action: BreakAction::Taken,
                banked_min: 0,
                duration_min: bank,
                spend_min: None,
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
            BreakKind::Bank => 0,
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
            spend_min: None,
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

    // -----------------------------------------------------------------------
    // План дня (v0.2, PRD FR-14…FR-18). Операции только валидируют и порождают
    // события; чекбоксы «☀️ Сегодня» проставляет vault-синк (PlanTaskSynced).
    // -----------------------------------------------------------------------

    /// Импорт/пересборка плана из «☀️ Сегодня» (заменяет задачи, встречи хранит).
    pub fn op_build_plan(
        &self,
        day: NaiveDate,
        tasks: Vec<PlanTask>,
        today_note_snapshot: Option<String>,
    ) -> Result<Vec<Event>, OpError> {
        if day != self.current_date {
            return Err(OpError::InvalidState("план собирается на сегодняшний день".into()));
        }
        Ok(vec![PlanBuilt { day, tasks, today_note_snapshot }.into()])
    }

    fn require_plan(&self) -> Result<(), OpError> {
        self.plan.as_ref().map(|_| ()).ok_or_else(|| OpError::InvalidState("план дня ещё не собран".into()))
    }

    /// Ручное добавление задачи в план.
    pub fn op_add_plan_task(
        &self,
        section: PlanSection,
        title: String,
        est_pomodoros: Option<u32>,
    ) -> Result<Vec<Event>, OpError> {
        let title = title.trim().to_string();
        if title.is_empty() {
            return Err(OpError::InvalidState("пустая задача".into()));
        }
        self.require_plan()?;
        Ok(vec![PlanTaskAdded {
            task: PlanTask {
                id: Uuid::new_v4(),
                section,
                title,
                wiki_links: Vec::new(),
                marker: None,
                today_note_line: None,
                est_pomodoros,
                done: false,
                today_note_done: false,
            },
        }
        .into()])
    }

    pub fn op_remove_plan_task(&self, task_id: Uuid) -> Result<Vec<Event>, OpError> {
        let known = self.plan.as_ref().is_some_and(|p| p.tasks.iter().any(|t| t.id == task_id));
        if !known {
            return Err(OpError::InvalidState("задача плана не найдена".into()));
        }
        Ok(vec![PlanTaskRemoved { task_id }.into()])
    }

    /// Раскладка по секциям и оценка в помидорках (FR-14, FR-16).
    pub fn op_edit_plan_task(
        &self,
        task_id: Uuid,
        section: Option<PlanSection>,
        est_pomodoros: Option<u32>,
    ) -> Result<Vec<Event>, OpError> {
        let known = self.plan.as_ref().is_some_and(|p| p.tasks.iter().any(|t| t.id == task_id));
        if !known {
            return Err(OpError::InvalidState("задача плана не найдена".into()));
        }
        if section.is_none() && est_pomodoros.is_none() {
            return Ok(Vec::new());
        }
        Ok(vec![PlanTaskEdited { task_id, section, est_pomodoros }.into()])
    }

    /// Отметка выполненной в приложении; чекбокс догонит через vault-синк.
    pub fn op_set_plan_task_done(&self, task_id: Uuid, done: bool) -> Result<Vec<Event>, OpError> {
        let task = self
            .plan
            .as_ref()
            .and_then(|p| p.tasks.iter().find(|t| t.id == task_id))
            .ok_or_else(|| OpError::InvalidState("задача плана не найдена".into()))?;
        if task.done == done {
            return Ok(Vec::new());
        }
        Ok(vec![PlanTaskCompleted { task_id, done }.into()])
    }

    /// Факт синхронизации чекбокса «☀️ Сегодня» (вызывает vault-синк).
    pub fn op_plan_task_synced(
        &self,
        task_id: Uuid,
        today_note_done: bool,
        conflict: Option<String>,
    ) -> Result<Vec<Event>, OpError> {
        self.require_plan()?;
        Ok(vec![PlanTaskSynced { task_id, today_note_done, conflict }.into()])
    }

    /// Ручная встреча дня (FR-17).
    pub fn op_add_meeting(
        &self,
        title: String,
        starts_at: Option<DateTime<Local>>,
        duration_min: Option<i64>,
    ) -> Result<Vec<Event>, OpError> {
        let title = title.trim().to_string();
        if title.is_empty() {
            return Err(OpError::InvalidState("пустая тема встречи".into()));
        }
        self.require_plan()?;
        Ok(vec![MeetingAdded {
            meeting: PlanMeeting { id: Uuid::new_v4(), title, starts_at, duration_min, happened: false },
        }
        .into()])
    }

    /// Вечерняя отметка: состоялась / перенесена.
    pub fn op_set_meeting_happened(&self, meeting_id: Uuid, happened: bool) -> Result<Vec<Event>, OpError> {
        let known = self.plan.as_ref().is_some_and(|p| p.meetings.iter().any(|m| m.id == meeting_id));
        if !known {
            return Err(OpError::InvalidState("встреча не найдена".into()));
        }
        Ok(vec![MeetingHappened { meeting_id, happened }.into()])
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
        if self.day_closed {
            return Err(OpError::InvalidState("день уже закрыт".into()));
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
                    plan_task_id: e.plan_task_id,
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
                    BreakKind::Bank => 0,
                };
                match e.action {
                    BreakAction::Taken => {
                        match e.spend_min {
                            // «Разрыв = отдых»: списать ровно spend минут из банка
                            // (сначала длинного, потом короткого).
                            Some(spend) => {
                                let from_long = spend.min(self.long_extra_min);
                                self.long_extra_min -= from_long;
                                self.short_extra_min -= (spend - from_long).min(self.short_extra_min);
                            }
                            // Длительность уже включает банк этого вида — банк потреблён.
                            None => match e.kind {
                                BreakKind::Short => self.short_extra_min = 0,
                                BreakKind::Long => self.long_extra_min = 0,
                                BreakKind::Bank => {
                                    self.short_extra_min = 0;
                                    self.long_extra_min = 0;
                                }
                            },
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
                    plan_task_id: None,
                    pauses: Vec::new(),
                });
            }
            E::DayClosed(e) => {
                self.short_extra_min = 0;
                self.long_extra_min = 0;
                self.streak = 0;
                self.day_closed = true;
                self.burned_bank_min = e.burned_min;
                let _ = e;
            }
            E::PlanBuilt(e) => {
                let meetings = match &self.plan {
                    // Пересборка хранит вручную добавленные встречи того же дня.
                    Some(existing) if existing.day == e.day => existing.meetings.clone(),
                    _ => Vec::new(),
                };
                self.plan = Some(DayPlan { day: e.day, tasks: e.tasks, meetings });
            }
            E::PlanTaskAdded(e) => {
                if let Some(plan) = &mut self.plan
                    && !plan.tasks.iter().any(|t| t.id == e.task.id) {
                        plan.tasks.push(e.task);
                    }
            }
            E::PlanTaskRemoved(e) => {
                if let Some(plan) = &mut self.plan {
                    plan.tasks.retain(|t| t.id != e.task_id);
                }
            }
            E::PlanTaskEdited(e) => {
                if let Some(task) = self
                    .plan
                    .as_mut()
                    .and_then(|p| p.tasks.iter_mut().find(|t| t.id == e.task_id))
                {
                    if let Some(section) = e.section {
                        task.section = section;
                    }
                    if let Some(est) = e.est_pomodoros {
                        task.est_pomodoros = Some(est);
                    }
                }
            }
            E::PlanTaskCompleted(e) => {
                if let Some(task) = self
                    .plan
                    .as_mut()
                    .and_then(|p| p.tasks.iter_mut().find(|t| t.id == e.task_id))
                {
                    task.done = e.done;
                }
            }
            E::PlanTaskSynced(e) => {
                if let Some(task) = self
                    .plan
                    .as_mut()
                    .and_then(|p| p.tasks.iter_mut().find(|t| t.id == e.task_id))
                    && e.conflict.is_none()
                {
                    task.today_note_done = e.today_note_done;
                }
            }
            E::MeetingAdded(e) => {
                if let Some(plan) = &mut self.plan
                    && !plan.meetings.iter().any(|m| m.id == e.meeting.id) {
                        plan.meetings.push(e.meeting);
                    }
            }
            E::MeetingHappened(e) => {
                if let Some(meeting) = self
                    .plan
                    .as_mut()
                    .and_then(|p| p.meetings.iter_mut().find(|m| m.id == e.meeting_id))
                {
                    meeting.happened = e.happened;
                }
            }
            E::VaultSyncStatus(_) => {}
        }
        if ts.date_naive() > self.current_date {
            self.current_date = ts.date_naive();
            self.day_closed = false;
            self.burned_bank_min = 0;
            // План — дневные данные: в новый день собирается заново из «Сегодня».
            self.plan = None;
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
        self.day_closed = false;
        self.burned_bank_min = 0;
        self.plan = None;
        Vec::new()
    }

    /// События для обнаруженного разрыва (FR-4). Обычный ответ — одна запись
    /// `GapLogged`. Ответ «это был отдых» (v0.1.1): из банка списываются минуты
    /// под отдых (`BreakEvent` с `spend_min`), остаток сверх банка — обычный
    /// разрыв; при пустом банке поведение совпадает с обычным ответом.
    fn gap_events(&self, gap: GapWindow, as_rest: bool) -> Vec<Event> {
        if !as_rest {
            return vec![GapLogged { started_at: gap.0, ended_at: gap.1, reason: gap.2 }.into()];
        }
        let total_min = (gap.1 - gap.0).num_minutes();
        let rest_min = total_min.min(self.bank_min());
        let mut events = Vec::new();
        if rest_min > 0 {
            events.push(
                BreakEvent {
                    kind: BreakKind::Long,
                    action: BreakAction::Taken,
                    banked_min: self.bank_min() - rest_min,
                    duration_min: rest_min,
                    spend_min: Some(rest_min),
                }
                .into(),
            );
        }
        if total_min > rest_min {
            events.push(
                GapLogged {
                    started_at: gap.0,
                    ended_at: gap.1 - chrono::Duration::minutes(rest_min),
                    reason: gap.2,
                }
                .into(),
            );
        }
        events
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
