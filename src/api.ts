// Обёртки над Tauri-командами и типы, зеркалящие serde-структуры Rust (snake_case).
import { invoke } from "@tauri-apps/api/core";

export type PauseReason = "internal" | "external" | "away";
export type Category = "work" | "personal" | "meeting";
export type BreakKind = "short" | "long" | "bank";
export type BreakAction = "taken" | "skip_to_short" | "skip_to_long" | "skip_plain";
export type ThoughtResolution = "note" | "task_draft" | "dismissed";
export type SessionStatus = "running" | "paused" | "completed" | "partial" | "void";
export type SessionSource = "timer" | "manual";
export type PlanSection = "work" | "personal";

export interface UiPause {
  started_at: string;
  ended_at: string | null;
  reason: PauseReason | null;
}

export interface UiSession {
  id: string;
  started_at: string;
  ended_at: string | null;
  planned_min: number;
  actual_min: number | null;
  status: SessionStatus;
  category: Category | null;
  confidential: boolean;
  note: string;
  label: string | null;
  source: SessionSource;
  plan_task_id: string | null;
  pauses: UiPause[];
}

export interface UiGap {
  started_at: string;
  ended_at: string;
  reason: string;
}

export interface UiThought {
  id: string;
  created_at: string;
  text: string;
  resolved: boolean;
  resolution: ThoughtResolution | null;
}

export interface UiBreakEvent {
  at: string;
  kind: BreakKind;
  action: BreakAction;
  banked_min: number;
  duration_min: number;
}

export type PhaseView =
  | { kind: "idle" }
  | {
      kind: "focus";
      session_id: string;
      label: string | null;
      planned_min: number;
      elapsed_secs: number;
      paused: boolean;
      confidential: boolean;
      pauses: UiPause[];
    }
  | { kind: "break"; break_kind: BreakKind; remaining_secs: number; duration_min: number };

export interface ProposedBreak {
  kind: BreakKind;
  duration_min: number;
}

export interface UiPlanTask {
  id: string;
  section: PlanSection;
  title: string;
  wiki_links: string[];
  marker: string | null;
  est_pomodoros: number | null;
  done: boolean;
  today_note_done: boolean;
  from_today_note: boolean;
  fact_pomodoros: number;
}

export interface UiPlanMeeting {
  id: string;
  title: string;
  starts_at: string | null;
  duration_min: number | null;
  happened: boolean;
}

export interface UiPlan {
  day: string;
  tasks: UiPlanTask[];
  meetings: UiPlanMeeting[];
}

export interface Snapshot {
  phase: PhaseView;
  bank_min: number;
  streak: number;
  day: string;
  day_closed: boolean;
  burned_bank_min: number;
  plan: UiPlan | null;
  sessions: UiSession[];
  gaps: UiGap[];
  thoughts: UiThought[];
  breaks: UiBreakEvent[];
  proposed_break: ProposedBreak | null;
}

export interface Settings {
  timer: {
    focus_min: number;
    short_break_min: number;
    long_break_min: number;
    long_every: number;
    gap_threshold_min: number;
  };
  autostart: boolean;
  db_online: boolean;
}

/// Ошибка команды: структурированный вопрос о разрыве или текст.
export type CmdError = { kind: "gap_needed"; minutes: number } | { kind: "message"; text: string };

export function isCmdError(e: unknown): e is CmdError {
  return typeof e === "object" && e !== null && "kind" in e;
}

export const api = {
  getSnapshot: () => invoke<Snapshot>("get_snapshot"),
  dbStatus: () => invoke<boolean>("db_status"),
  startFocus: (
    label: string | null,
    confidential: boolean,
    gap_reason: string | null,
    gap_as_rest: boolean,
    category: Category | null,
    plan_task_id?: string | null,
  ) =>
    invoke<Snapshot>("start_focus", {
      label,
      confidential,
      gapReason: gap_reason,
      gapAsRest: gap_as_rest,
      category,
      planTaskId: plan_task_id ?? null,
    }),
  pauseFocus: () => invoke<Snapshot>("pause_focus"),
  resumeFocus: (reason: PauseReason) => invoke<Snapshot>("resume_focus", { reason }),
  completeFocus: () => invoke<Snapshot>("complete_focus"),
  voidSession: (reason: string | null) => invoke<Snapshot>("void_session", { reason }),
  startBreak: (gap_reason: string | null, gap_as_rest: boolean) =>
    invoke<Snapshot>("start_break", { gapReason: gap_reason, gapAsRest: gap_as_rest }),
  startBankBreak: (gap_reason: string | null, gap_as_rest: boolean) =>
    invoke<Snapshot>("start_bank_break", { gapReason: gap_reason, gapAsRest: gap_as_rest }),
  skipBreak: (action: BreakAction) => invoke<Snapshot>("skip_break", { action }),
  endBreak: () => invoke<Snapshot>("end_break"),
  parkThought: (text: string) => invoke<Snapshot>("park_thought", { text }),
  resolveThought: (thoughtId: string, resolution: ThoughtResolution) =>
    invoke<Snapshot>("resolve_thought", { thoughtId, resolution }),
  setNote: (sessionId: string, note: string) => invoke<Snapshot>("set_note", { sessionId, note }),
  setCategory: (sessionId: string, category: Category) => invoke<Snapshot>("set_category", { sessionId, category }),
  addManualSession: (
    startedAt: string,
    minutes: number,
    label: string | null,
    note: string | null,
    category: Category | null,
  ) => invoke<Snapshot>("add_manual_session", { startedAt, minutes, label, note, category }),
  closeDay: () => invoke<Snapshot>("close_day"),
  buildPlanFromToday: () => invoke<Snapshot>("build_plan_from_today"),
  addPlanTask: (section: PlanSection, title: string, estPomodoros: number | null) =>
    invoke<Snapshot>("add_plan_task", { section, title, estPomodoros }),
  removePlanTask: (taskId: string) => invoke<Snapshot>("remove_plan_task", { taskId }),
  editPlanTask: (taskId: string, section: PlanSection | null, estPomodoros: number | null) =>
    invoke<Snapshot>("edit_plan_task", { taskId, section, estPomodoros }),
  setPlanTaskDone: (taskId: string, done: boolean) => invoke<Snapshot>("set_plan_task_done", { taskId, done }),
  addMeeting: (title: string, startsAt: string | null, durationMin: number | null) =>
    invoke<Snapshot>("add_meeting", { title, startsAt, durationMin }),
  setMeetingHappened: (meetingId: string, happened: boolean) =>
    invoke<Snapshot>("set_meeting_happened", { meetingId, happened }),
  getSettings: () => invoke<Settings>("get_settings"),
  saveSettings: (s: Settings["timer"], autostart: boolean) =>
    invoke<Settings>("save_settings", {
      focusMin: s.focus_min,
      shortBreakMin: s.short_break_min,
      longBreakMin: s.long_break_min,
      longEvery: s.long_every,
      gapThresholdMin: s.gap_threshold_min,
      autostart,
    }),
};

// ---------------------------------------------------------------------------
// Форматирование
// ---------------------------------------------------------------------------

export function fmtClock(secs: number): string {
  const s = Math.max(0, Math.floor(secs));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

export function fmtTime(iso: string): string {
  const d = new Date(iso);
  return d.toLocaleTimeString("ru-RU", { hour: "2-digit", minute: "2-digit" });
}

export const STATUS_LABEL: Record<SessionStatus, string> = {
  running: "идёт",
  paused: "пауза",
  completed: "завершена",
  partial: "частично",
  void: "аннулирована",
};

export const CATEGORY_LABEL: Record<Category, string> = {
  work: "работа",
  personal: "личное",
  meeting: "встреча",
};

export const BREAK_KIND_LABEL: Record<BreakKind, string> = {
  short: "Короткий перерыв",
  long: "Длинный перерыв",
  bank: "Отдых из банка",
};

export function breakIcon(kind: BreakKind): string {
  return kind === "long" ? "☕" : kind === "bank" ? "🛌" : "💬";
}

export const PLAN_SECTION_LABEL: Record<PlanSection, string> = {
  work: "работа",
  personal: "личное",
};

export const PAUSE_REASON_LABEL: Record<PauseReason, string> = {
  internal: "сам отвлёкся",
  external: "отвлекли",
  away: "отошёл",
};

export const RESOLUTION_LABEL: Record<ThoughtResolution, string> = {
  note: "в заметку",
  task_draft: "черновик задачи",
  dismissed: "убрана",
};
