-- TimeShift — начальная схема (docs/ARCHITECTURE.md §4)

CREATE TABLE applied_events (        -- идемпотентность репликации WAL
  event_id UUID PRIMARY KEY,
  applied_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE focus_sessions (
  id UUID PRIMARY KEY,
  started_at TIMESTAMPTZ NOT NULL,
  ended_at TIMESTAMPTZ,
  planned_min INT NOT NULL,          -- 25 по умолчанию
  actual_min INT,                    -- чистое время фокуса без пауз
  status TEXT NOT NULL,              -- running | completed | partial | void
  category TEXT,                     -- work | personal | meeting | null (не определено)
  confidential BOOLEAN NOT NULL DEFAULT false,
  note TEXT,                         -- «что сделано»
  source TEXT NOT NULL DEFAULT 'timer',  -- timer | manual
  plan_task_id UUID                  -- FK day_plan_tasks, nullable
);

CREATE TABLE session_pauses (
  id UUID PRIMARY KEY,
  session_id UUID NOT NULL REFERENCES focus_sessions(id),
  started_at TIMESTAMPTZ NOT NULL,
  ended_at TIMESTAMPTZ,
  reason TEXT                        -- internal | external | away | null (ещё на паузе)
);

CREATE TABLE day_gaps (              -- ретро-логированные разрывы между этапами
  id UUID PRIMARY KEY,
  day DATE NOT NULL,
  started_at TIMESTAMPTZ NOT NULL,
  ended_at TIMESTAMPTZ NOT NULL,
  reason TEXT NOT NULL               -- обед | встреча | потерял_ритм | иное (текст)
);

CREATE TABLE parked_thoughts (
  id UUID PRIMARY KEY,
  created_at TIMESTAMPTZ NOT NULL,
  text TEXT NOT NULL,
  resolved_at TIMESTAMPTZ,
  resolution TEXT                    -- note | task_draft | dismissed | null
);

CREATE TABLE day_plans (
  id UUID PRIMARY KEY,
  day DATE NOT NULL UNIQUE,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  today_note_snapshot TEXT           -- сырой текст «☀️ Сегодня» на момент сборки
);

CREATE TABLE day_plan_tasks (
  id UUID PRIMARY KEY,
  plan_id UUID NOT NULL REFERENCES day_plans(id),
  section TEXT NOT NULL,             -- work | personal | meeting
  title TEXT NOT NULL,
  wiki_links TEXT[],                 -- [[ссылки]] из строки задачи
  markers TEXT,                      -- ⏫/▲/⚡ из «Сегодня»
  today_note_line TEXT,              -- точная строка для обратной синхронизации чекбокса
  est_pomodoros INT,
  position INT NOT NULL,
  done BOOLEAN NOT NULL DEFAULT false,
  today_note_done BOOLEAN NOT NULL DEFAULT false  -- факт отметки в «Сегодня»
);

CREATE TABLE plan_meetings (
  id UUID PRIMARY KEY,
  plan_id UUID NOT NULL REFERENCES day_plans(id),
  title TEXT NOT NULL,
  starts_at TIMESTAMPTZ,
  duration_min INT,
  happened BOOLEAN                   -- отметка вечером: состоялась/перенесена
);

CREATE TABLE break_events (
  id UUID PRIMARY KEY,
  at TIMESTAMPTZ NOT NULL,
  kind TEXT NOT NULL,                -- short | long
  action TEXT NOT NULL,              -- taken | skip_to_short | skip_to_long | skip_plain
  banked_min INT NOT NULL DEFAULT 0, -- состояние банка после события
  burned_min INT                     -- сколько сгорело (при сгорании в конце дня)
);

CREATE TABLE llm_reviews (
  id UUID PRIMARY KEY,
  day DATE NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  endpoint_model TEXT NOT NULL,
  prompt_tokens INT, completion_tokens INT,
  result TEXT NOT NULL               -- текст обзора, дописанный в фокус-лог
);

CREATE INDEX idx_focus_sessions_started_at ON focus_sessions (started_at);
CREATE INDEX idx_session_pauses_session ON session_pauses (session_id);
CREATE INDEX idx_day_plan_tasks_plan ON day_plan_tasks (plan_id);
