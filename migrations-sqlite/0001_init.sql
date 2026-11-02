-- TimeShift — SQLite-схема реплики WAL (v0.3 «Модули»).
-- Зеркало migrations/0001_init.sql + 0002_session_label.sql для Postgres:
-- те же таблицы, колонки и строковые коды, но TEXT вместо UUID/TIMESTAMPTZ
-- (RFC-3339/ISO-даты), INTEGER вместо BOOLEAN, JSON-текст вместо TEXT[].
-- Идемпотентность репликации — та же `applied_events`.

CREATE TABLE applied_events (
  event_id TEXT PRIMARY KEY,
  applied_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE focus_sessions (
  id TEXT PRIMARY KEY,
  started_at TEXT NOT NULL,
  ended_at TEXT,
  planned_min INTEGER NOT NULL,      -- 25 по умолчанию
  actual_min INTEGER,                -- чистое время фокуса без пауз
  status TEXT NOT NULL,              -- running | completed | partial | void
  category TEXT,                     -- work | personal | meeting | null
  confidential INTEGER NOT NULL DEFAULT 0,
  note TEXT,                         -- «что сделано»
  source TEXT NOT NULL DEFAULT 'timer',  -- timer | manual
  label TEXT,                        -- свободная подпись задачи
  void_reason TEXT,                  -- причина аннулирования
  plan_task_id TEXT                  -- FK day_plan_tasks, nullable
);

CREATE TABLE session_pauses (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES focus_sessions(id),
  started_at TEXT NOT NULL,
  ended_at TEXT,
  reason TEXT                        -- internal | external | away | null
);

CREATE TABLE day_gaps (              -- ретро-логированные разрывы между этапами
  id TEXT PRIMARY KEY,
  day TEXT NOT NULL,                 -- ГГГГ-ММ-ДД
  started_at TEXT NOT NULL,
  ended_at TEXT NOT NULL,
  reason TEXT NOT NULL
);

CREATE TABLE parked_thoughts (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  text TEXT NOT NULL,
  resolved_at TEXT,
  resolution TEXT                    -- note | task_draft | dismissed | null
);

CREATE TABLE day_plans (
  id TEXT PRIMARY KEY,
  day TEXT NOT NULL UNIQUE,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  today_note_snapshot TEXT           -- сырой текст «☀️ Сегодня» на момент сборки
);

CREATE TABLE day_plan_tasks (
  id TEXT PRIMARY KEY,
  plan_id TEXT NOT NULL REFERENCES day_plans(id),
  section TEXT NOT NULL,             -- work | personal | meeting
  title TEXT NOT NULL,
  wiki_links TEXT,                   -- JSON-массив [[ссылок]] (в Postgres — TEXT[])
  markers TEXT,                      -- ⏫/▲/⚡ из «Сегодня»
  today_note_line TEXT,              -- точная строка для обратной синхронизации чекбокса
  est_pomodoros INTEGER,
  position INTEGER NOT NULL,
  done INTEGER NOT NULL DEFAULT 0,
  today_note_done INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE plan_meetings (
  id TEXT PRIMARY KEY,
  plan_id TEXT NOT NULL REFERENCES day_plans(id),
  title TEXT NOT NULL,
  starts_at TEXT,
  duration_min INTEGER,
  happened INTEGER                   -- отметка вечером: состоялась/перенесена
);

CREATE TABLE break_events (
  id TEXT PRIMARY KEY,
  at TEXT NOT NULL,
  kind TEXT NOT NULL,                -- short | long | bank | day_close
  action TEXT NOT NULL,              -- taken | skip_to_short | skip_to_long | skip_plain | burn
  banked_min INTEGER NOT NULL DEFAULT 0,
  burned_min INTEGER
);

CREATE TABLE llm_reviews (
  id TEXT PRIMARY KEY,
  day TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  endpoint_model TEXT NOT NULL,
  prompt_tokens INTEGER, completion_tokens INTEGER,
  result TEXT NOT NULL               -- текст обзора, дописанный в фокус-лог
);

CREATE INDEX idx_focus_sessions_started_at ON focus_sessions (started_at);
CREATE INDEX idx_session_pauses_session ON session_pauses (session_id);
CREATE INDEX idx_day_plan_tasks_plan ON day_plan_tasks (plan_id);
