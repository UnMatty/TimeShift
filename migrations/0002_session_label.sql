-- TimeShift — подпись задачи и причина аннулирования (v0.1, WP-2)
-- `label` — свободная подпись задачи на сессии (привязка к плану дня — plan_task_id в v0.2);
-- `void_reason` — необязательная причина аннулирования (событие session_voided).

ALTER TABLE focus_sessions ADD COLUMN label TEXT;
ALTER TABLE focus_sessions ADD COLUMN void_reason TEXT;
