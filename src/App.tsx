// TimeShift UI v0.1 (WP-5): экраны «День», «Парковка», «Настройки».
// Все операции идут через api.ts; состояние приходит событием tick из Rust.
import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  api,
  breakIcon,
  BREAK_KIND_LABEL,
  CATEGORY_LABEL,
  isCmdError,
  PAUSE_REASON_LABEL,
  PLAN_SECTION_LABEL,
  RESOLUTION_LABEL,
  STATUS_LABEL,
  fmtClock,
  fmtTime,
} from "./api";
import type { Category, PauseReason, PlanSection, Settings, Snapshot, UiSession } from "./api";
import "./App.css";

type Tab = "day" | "plan" | "parking" | "settings";

export default function App() {
  const [tab, setTab] = useState<Tab>("day");
  const [snap, setSnap] = useState<Snapshot | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Вопрос о разрыве (FR-4): ждёт ответа «где был?» перед стартом этапа.
  const [gap, setGap] = useState<{
    minutes: number;
    run: (reason: string, asRest: boolean) => void;
  } | null>(null);

  // Операция — функция от причины разрыва: при GapNeeded она вызовется повторно.
  const run = useCallback(async (fn: (gapReason?: string, asRest?: boolean) => Promise<Snapshot>) => {
    try {
      setSnap(await fn(undefined, false));
      setError(null);
    } catch (e) {
      if (isCmdError(e)) {
        if (e.kind === "gap_needed") {
          setGap({
            minutes: e.minutes,
            run: (reason, asRest) => {
              setGap(null);
              void run(() => fn(reason, asRest));
            },
          });
        } else {
          setError(e.text);
        }
      } else {
        setError(String(e));
      }
    }
  }, []);

  useEffect(() => {
    void api.getSnapshot().then(setSnap).catch(String);
    const unlisteners = [
      listen<Snapshot>("tick", (e) => setSnap(e.payload)),
      listen("park-hotkey", () => setTab("parking")),
    ];
    return () => void unlisteners.forEach((p) => void p.then((un) => un()));
  }, []);

  if (!snap) {
    return <main className="loading">Загрузка…</main>;
  }

  return (
    <main className="app">
      <header className="topbar">
        <nav>
          {(
            [
              ["day", "День"],
              ["plan", `План${snap.plan ? "" : " ●"}`],
              ["parking", `Парковка${snap.thoughts.some((t) => !t.resolved) ? " ●" : ""}`],
              ["settings", "Настройки"],
            ] as const
          ).map(([key, label]) => (
            <button key={key} className={tab === key ? "tab active" : "tab"} onClick={() => setTab(key)}>
              {label}
            </button>
          ))}
        </nav>
        <DbStatus />
      </header>

      {error && (
        <div className="error" onClick={() => setError(null)}>
          {error} — нажмите, чтобы скрыть
        </div>
      )}

      {gap && (
        <GapDialog
          minutes={gap.minutes}
          bankMin={snap.bank_min}
          onAnswer={(reason, asRest) => gap.run(reason, asRest)}
        />
      )}

      {tab === "day" && <DayTab snap={snap} run={run} />}
      {tab === "plan" && <PlanTab snap={snap} run={run} />}
      {tab === "parking" && <ParkingTab snap={snap} run={run} />}
      {tab === "settings" && <SettingsTab />}
    </main>
  );
}

// ---------------------------------------------------------------------------

function DbStatus() {
  const [online, setOnline] = useState<boolean | null>(null);
  useEffect(() => {
    void api.dbStatus().then(setOnline).catch(() => setOnline(false));
    const t = setInterval(() => void api.dbStatus().then(setOnline).catch(() => setOnline(false)), 10_000);
    return () => clearInterval(t);
  }, []);
  return (
    <span className={online ? "db online" : "db offline"}>
      {online === null ? "…" : online ? "БД онлайн" : "БД офлайн — журнал копится"}
    </span>
  );
}

function GapDialog({
  minutes,
  bankMin,
  onAnswer,
}: {
  minutes: number;
  bankMin: number;
  onAnswer: (reason: string, asRest: boolean) => void;
}) {
  const [reason, setReason] = useState("обед");
  const [other, setOther] = useState("");
  const restMin = Math.min(minutes, bankMin);
  const answer = (asRest: boolean) => onAnswer(other.trim() || reason, asRest);
  return (
    <div className="overlay">
      <div className="dialog">
        <h3>Разрыв {minutes} минут</h3>
        <p>Между этапами прошло больше порога. Чем занимался?</p>
        <div className="chips">
          {["обед", "встреча", "потерял ритм"].map((r) => (
            <button key={r} className={reason === r ? "chip active" : "chip"} onClick={() => setReason(r)}>
              {r}
            </button>
          ))}
        </div>
        <input
          placeholder="другое — своими словами"
          value={other}
          onChange={(e) => setOther(e.currentTarget.value)}
        />
        <div className="row">
          <button className="primary" onClick={() => answer(false)}>
            Записать разрыв
          </button>
          {restMin > 0 && (
            <button onClick={() => answer(true)}>☕ Это был отдых — зачесть {restMin} мин из банка</button>
          )}
        </div>
        {restMin > 0 && restMin < minutes && (
          <p className="muted small">Банк покроет {restMin} мин, остаток запишется разрывом.</p>
        )}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// День
// ---------------------------------------------------------------------------

function DayTab({
  snap,
  run,
}: {
  snap: Snapshot;
  run: (fn: (gapReason?: string, asRest?: boolean) => Promise<Snapshot>) => Promise<void>;
}) {
  return (
    <>
      <SummaryCard snap={snap} />
      <PhaseCard snap={snap} run={run} />
      <SessionsCard snap={snap} run={run} />
      <ManualCard run={run} />
      <footer className="day-footer">
        {snap.day_closed ? (
          <span className="closed-note">
            ✅ День закрыт — несписанный отдых ({snap.bank_min} мин) сгорел. Новый день начнётся сам.
          </span>
        ) : (
          <button onClick={() => void run(() => api.closeDay())}>🌙 Завершить день (сжечь банк отдыха)</button>
        )}
      </footer>
    </>
  );
}

/// Итоги дня одним взглядом: завершённые сессии, чистый фокус, отдых, банк.
function SummaryCard({ snap }: { snap: Snapshot }) {
  const done = snap.sessions.filter((s) => s.status === "completed" || s.status === "partial");
  const focusMin = done.reduce((a, s) => a + (s.actual_min ?? 0), 0);
  const restMin = snap.breaks.filter((b) => b.action === "taken").reduce((a, b) => a + b.duration_min, 0);
  return (
    <section className="card summary">
      <span>
        🍅 <b>{done.length}</b>
      </span>
      <span>
        фокус <b>{focusMin}</b> мин
      </span>
      <span>
        отдых <b>{restMin}</b> мин
      </span>
      <span>
        банк <b>{snap.bank_min}</b> мин
      </span>
      {snap.gaps.length > 0 && (
        <span>
          разрывов <b>{snap.gaps.length}</b>
        </span>
      )}
    </section>
  );
}

function PhaseCard({
  snap,
  run,
}: {
  snap: Snapshot;
  run: (fn: (gapReason?: string, asRest?: boolean) => Promise<Snapshot>) => Promise<void>;
}) {
  const [label, setLabel] = useState("");
  const [confidential, setConfidential] = useState(false);
  const [category, setCategory] = useState<Category | null>(null);
  const [pauseReason, setPauseReason] = useState<PauseReason>("internal");
  // Подтверждение «старт фокуса без отдыха», когда перерыв ещё не взят.
  const [confirmSkip, setConfirmSkip] = useState(false);

  const phase = snap.phase;
  const proposed = snap.proposed_break;
  const bank = snap.bank_min;

  const takeBreak = () => void run((gr, asRest) => api.startBreak(gr ?? null, asRest ?? false));
  const takeBankBreak = () => void run((gr, asRest) => api.startBankBreak(gr ?? null, asRest ?? false));
  const startFocus = () =>
    void run((gr, asRest) => api.startFocus(label || null, confidential, gr ?? null, asRest ?? false, category));
  // Скип предложенного перерыва в банк и сразу старт фокуса.
  const skipAndStartFocus = () =>
    void run(async (gr, asRest) => {
      await api.skipBreak(proposed?.kind === "long" ? "skip_to_long" : "skip_to_short");
      return api.startFocus(label || null, confidential, gr ?? null, asRest ?? false, category);
    });

  // Блок «над чем работаешь»: подпись, категория, конфиденциальность, старт.
  const focusStart = (primary: boolean) => (
    <div className="row focus-start">
      <input
        className="grow"
        placeholder="над чем работаешь (необязательно)"
        value={label}
        onChange={(e) => setLabel(e.currentTarget.value)}
      />
      <div className="chips">
        {(Object.keys(CATEGORY_LABEL) as Category[]).map((k) => (
          <button
            key={k}
            className={category === k ? "chip active" : "chip"}
            onClick={() => setCategory(category === k ? null : k)}
          >
            {CATEGORY_LABEL[k]}
          </button>
        ))}
      </div>
      <label className="check">
        <input
          type="checkbox"
          checked={confidential}
          onChange={(e) => setConfidential(e.currentTarget.checked)}
        />
        конфиденциально
      </label>
      <button
        className={primary ? "primary btn-lg" : ""}
        onClick={() => (proposed ? setConfirmSkip(true) : startFocus())}
      >
        🍅 Старт фокуса
      </button>
    </div>
  );

  return (
    <section className="card phase-card">
      {confirmSkip && proposed && (
        <div className="overlay">
          <div className="dialog">
            <h3>Перерыв {proposed.duration_min} мин не взят</h3>
            <p>
              Начать помидорку без отдыха? Пропущенные минуты уйдут в банк — он станет {bank + proposed.duration_min}{" "}
              мин и сгорит при закрытии дня.
            </p>
            <div className="row">
              <button className="primary" onClick={skipAndStartFocus}>
                ⏭ Скипнуть в банк и начать фокус
              </button>
              <button
                onClick={() => {
                  setConfirmSkip(false);
                  takeBreak();
                }}
              >
                ☕ Всё-таки отдохнуть
              </button>
              <button onClick={() => setConfirmSkip(false)}>Отмена</button>
            </div>
          </div>
        </div>
      )}

      {phase.kind === "idle" && (
        <div className="phase idle">
          <div className="big">◦</div>
          <div className="phase-info">
            <h2>Свободен</h2>
            <p className="muted">
              Стрик: {snap.streak} 🍅 · Банк отдыха: {bank} мин
              {snap.gaps.length > 0 && <> · Разрывов записано: {snap.gaps.length}</>}
            </p>

            {proposed ? (
              // Отдых — ожидаемое действие после помидорки: первичная кнопка.
              <div className="rest-block">
                <div className="row">
                  <button className="primary btn-lg" onClick={takeBreak}>
                    {breakIcon(proposed.kind)} {BREAK_KIND_LABEL[proposed.kind]} — {proposed.duration_min} мин
                  </button>
                  {bank > 0 && (
                    <button className="btn-lg" onClick={takeBankBreak}>
                      🛌 Весь банк ({bank} мин)
                    </button>
                  )}
                </div>
                <div className="row skip-row">
                  <span className="muted small">Не отдыхать:</span>
                  <button onClick={() => void run(() => api.skipBreak("skip_to_short"))}>⏭ в короткий</button>
                  <button onClick={() => void run(() => api.skipBreak("skip_to_long"))}>⏭ в длинный</button>
                  <button onClick={() => void run(() => api.skipBreak("skip_plain"))}>⏭ просто скип</button>
                </div>
              </div>
            ) : (
              bank > 0 && (
                <div className="row rest-block">
                  <button className="btn-lg" onClick={takeBankBreak}>
                    🛌 Отдохнуть весь банк ({bank} мин)
                  </button>
                </div>
              )
            )}

            {focusStart(!proposed)}
          </div>
        </div>
      )}

      {phase.kind === "focus" && (
        <div className="phase focus">
          <div className="big">{phase.paused ? "⏸" : "🍅"}</div>
          <div className="phase-info">
            <h2>
              {fmtClock(phase.planned_min * 60 - phase.elapsed_secs)}
              <span className="muted"> / {phase.planned_min}:00</span>
            </h2>
            <p className="muted">
              {phase.label || "без подписи"}
              {phase.confidential && " · 🔒 конфиденциально"}
              {" · стрик "}
              {snap.streak}
            </p>
            {phase.paused ? (
              <div className="row">
                <select value={pauseReason} onChange={(e) => setPauseReason(e.currentTarget.value as PauseReason)}>
                  {Object.entries(PAUSE_REASON_LABEL).map(([k, v]) => (
                    <option key={k} value={k}>
                      {v}
                    </option>
                  ))}
                </select>
                <button
                  className="primary"
                  onClick={() => void run(() => api.resumeFocus(pauseReason))}
                >
                  ▶️ Продолжить
                </button>
              </div>
            ) : (
              <div className="row">
                <button onClick={() => void run(() => api.pauseFocus())}>⏸ Пауза</button>
                <button className="primary" onClick={() => void run(() => api.completeFocus())}>
                  ✅ Завершить сейчас
                </button>
                <button className="danger" onClick={() => void run(() => api.voidSession(null))}>
                  🚫 Аннулировать
                </button>
              </div>
            )}
          </div>
        </div>
      )}

      {phase.kind === "break" && (
        <div className="phase break">
          <div className="big">{breakIcon(phase.break_kind)}</div>
          <div className="phase-info">
            <h2>
              {fmtClock(phase.remaining_secs)}
              <span className="muted"> / {phase.duration_min}:00</span>
            </h2>
            <p className="muted">{BREAK_KIND_LABEL[phase.break_kind]} — отойди от экрана</p>
            <div className="row">
              <button className="primary" onClick={() => void run(() => api.endBreak())}>
                ⏹ Закончить перерыв
              </button>
            </div>
          </div>
        </div>
      )}
    </section>
  );
}

const STATUS_CLASS: Record<UiSession["status"], string> = {
  running: "st-running",
  paused: "st-paused",
  completed: "st-done",
  partial: "st-partial",
  void: "st-void",
};

function SessionsCard({
  snap,
  run,
}: {
  snap: Snapshot;
  run: (fn: (gapReason?: string, asRest?: boolean) => Promise<Snapshot>) => Promise<void>;
}) {
  const sessions = [...snap.sessions].sort((a, b) => a.started_at.localeCompare(b.started_at));
  return (
    <section className="card">
      <h3>Сессии дня ({sessions.length})</h3>
      {sessions.length === 0 && <p className="muted">Пока пусто — стартуй первую помидорку.</p>}
      <div className="sessions">
        {sessions.map((s) => (
          <SessionRow key={s.id} s={s} run={run} />
        ))}
      </div>
    </section>
  );
}

function SessionRow({
  s,
  run,
}: {
  s: UiSession;
  run: (fn: (gapReason?: string, asRest?: boolean) => Promise<Snapshot>) => Promise<void>;
}) {
  const [noteOpen, setNoteOpen] = useState(false);
  const time = `${fmtTime(s.started_at)}${s.ended_at ? `–${fmtTime(s.ended_at)}` : "…"}`;
  return (
    <div className={`session ${STATUS_CLASS[s.status]}`}>
      <div className="s-time">{time}</div>
      <div className="s-main">
        <div className="s-title">
          {s.label || <i className="muted">без подписи</i>}
          <span className={`badge ${STATUS_CLASS[s.status]}`}>{STATUS_LABEL[s.status]}</span>
          {s.source === "manual" && <span className="badge">вручную</span>}
          {s.confidential && <span className="badge">🔒</span>}
          {s.actual_min !== null && <span className="muted"> · {s.actual_min} мин</span>}
          {s.pauses.length > 0 && (
            <span className="muted">
              {" · пауз "}
              {s.pauses.length}
              {s.pauses.some((p) => p.reason) && ": "}
              {s.pauses
                .filter((p) => p.reason)
                .map((p) => p.reason && PAUSE_REASON_LABEL[p.reason])
                .join(", ")}
            </span>
          )}
        </div>
        <button className={s.note ? "s-note" : "s-note empty"} onClick={() => setNoteOpen(true)}>
          {s.note || "＋ что сделано"}
        </button>
      </div>
      <select
        className="s-cat"
        value={s.category ?? ""}
        onChange={(e) => {
          const v = e.currentTarget.value as Category | "";
          void run(() => api.setCategory(s.id, v === "" ? "work" : v));
        }}
      >
        <option value="" disabled>
          — категория —
        </option>
        {Object.entries(CATEGORY_LABEL).map(([k, v]) => (
          <option key={k} value={k}>
            {v}
          </option>
        ))}
      </select>
      {noteOpen && (
        <NoteDialog
          title={`${time} · ${s.label || "без подписи"}`}
          initial={s.note}
          onClose={() => setNoteOpen(false)}
          onSave={(note) => {
            setNoteOpen(false);
            if (note !== s.note) void run(() => api.setNote(s.id, note));
          }}
        />
      )}
    </div>
  );
}

/// Большое поле «что сделано» (FR-8): заметка пишется в раскрытом окне,
/// а не в однострочном инпуте. Cmd+Enter — сохранить, Esc — закрыть.
function NoteDialog({
  title,
  initial,
  onClose,
  onSave,
}: {
  title: string;
  initial: string;
  onClose: () => void;
  onSave: (note: string) => void;
}) {
  const [text, setText] = useState(initial);
  const ref = useRef<HTMLTextAreaElement>(null);
  useEffect(() => ref.current?.focus(), []);
  return (
    <div className="overlay" onClick={onClose}>
      <div className="dialog note-dialog" onClick={(e) => e.stopPropagation()}>
        <h3>Что сделано</h3>
        <p>{title}</p>
        <textarea
          ref={ref}
          rows={6}
          placeholder="результат помидорки — своими словами"
          value={text}
          onChange={(e) => setText(e.currentTarget.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) onSave(text.trim());
            if (e.key === "Escape") onClose();
          }}
        />
        <div className="row">
          <button className="primary" onClick={() => onSave(text.trim())}>
            Сохранить
          </button>
          <button onClick={onClose}>Отмена</button>
          <span className="muted small">Cmd+Enter — сохранить, Esc — закрыть</span>
        </div>
      </div>
    </div>
  );
}

function ManualCard({ run }: { run: (fn: (gapReason?: string, asRest?: boolean) => Promise<Snapshot>) => Promise<void> }) {
  const [time, setTime] = useState("");
  const [minutes, setMinutes] = useState(25);
  const [label, setLabel] = useState("");
  const [category, setCategory] = useState<Category | "">("");
  const valid = /^\d{1,2}:\d{2}$/.test(time);
  return (
    <section className="card">
      <h3>Работал без приложения?</h3>
      <div className="row">
        <input placeholder="ЧЧ:ММ" value={time} onChange={(e) => setTime(e.currentTarget.value)} className="time" />
        <input
          type="number"
          min={1}
          max={240}
          value={minutes}
          onChange={(e) => setMinutes(Number(e.currentTarget.value) || 0)}
          className="time"
        />
        <input
          className="grow"
          placeholder="над чем работал"
          value={label}
          onChange={(e) => setLabel(e.currentTarget.value)}
        />
        <select value={category} onChange={(e) => setCategory(e.currentTarget.value as Category | "")}>
          <option value="">— категория —</option>
          {Object.entries(CATEGORY_LABEL).map(([k, v]) => (
            <option key={k} value={k}>
              {v}
            </option>
          ))}
        </select>
        <button
          disabled={!valid || minutes <= 0}
          onClick={() =>
            void run(() => api.addManualSession(time, minutes, label || null, null, category || null)).then(() => {
              setTime("");
              setLabel("");
            })
          }
        >
          + добавить
        </button>
      </div>
    </section>
  );
}

// ---------------------------------------------------------------------------
// План дня (v0.2): импорт из «☀️ Сегодня», секции, оценки, встречи
// ---------------------------------------------------------------------------

function PlanTab({
  snap,
  run,
}: {
  snap: Snapshot;
  run: (fn: (gapReason?: string, asRest?: boolean) => Promise<Snapshot>) => Promise<void>;
}) {
  const plan = snap.plan;
  const [newSection, setNewSection] = useState<PlanSection>("work");
  const [newTitle, setNewTitle] = useState("");
  const [newEst, setNewEst] = useState("");
  const [mTitle, setMTitle] = useState("");
  const [mTime, setMTime] = useState("");
  const [mMin, setMMin] = useState("");

  const startTask = (taskId: string) =>
    void run((gr, asRest) => api.startFocus(null, false, gr ?? null, asRest ?? false, null, taskId));
  const otherSection = (s: PlanSection): PlanSection => (s === "work" ? "personal" : "work");

  return (
    <>
      <section className="card">
        <div className="row plan-header">
          <h3 className="grow">
            План дня{plan && <span className="muted"> на {plan.day}</span>}
          </h3>
          <button className="primary" onClick={() => void run(() => api.buildPlanFromToday())}>
            ⟳ Собрать из «☀️ Сегодня»
          </button>
        </div>
        {!plan && (
          <p className="muted">
            План не собран. Импорт возьмёт все чекбоксы из «☀️ Сегодня» (секции «Встречи» и «Дедлайны» — мимо),
            раскладку по «работа/личное» и оценки в помидорках правишь здесь. Пересборка заменяет задачи,
            встречи и отметки остаются.
          </p>
        )}
      </section>

      {plan &&
        (["work", "personal"] as PlanSection[]).map((section) => {
          const tasks = plan.tasks.filter((t) => t.section === section);
          return (
            <section key={section} className="card">
              <h3>{section === "work" ? "💼 Работа" : "🏠 Личное"}</h3>
              <div className="sessions">
                {tasks.length === 0 && <p className="muted">Пусто — перенеси задачи или добавь свою внизу.</p>}
                {tasks.map((t) => (
                  <div key={t.id} className={`plan-task ${t.done ? "done" : ""}`}>
                    <input
                      type="checkbox"
                      checked={t.done}
                      onChange={(e) => void run(() => api.setPlanTaskDone(t.id, e.currentTarget.checked))}
                    />
                    <div className="s-main">
                      <div className="s-title">
                        {t.marker && <span className="muted">{t.marker} </span>}
                        {t.title}
                        {t.wiki_links.map((l) => (
                          <span key={l} className="badge">
                            {l.split("/").pop()}
                          </span>
                        ))}
                        {t.from_today_note &&
                          (t.today_note_done ? (
                            <span className="badge st-done">✓ в «Сегодня»</span>
                          ) : (
                            <span className="badge">синк чекбокса…</span>
                          ))}
                      </div>
                      <div className="muted small">
                        план {t.est_pomodoros ?? "—"} · факт {t.fact_pomodoros}
                      </div>
                    </div>
                    <button title="стартовать помидорку по задаче" disabled={t.done} onClick={() => startTask(t.id)}>
                      ▶ 🍅
                    </button>
                    <span className="est">
                      <button
                        title="минус помидорка из оценки"
                        onClick={() =>
                          void run(() => api.editPlanTask(t.id, null, Math.max((t.est_pomodoros ?? 0) - 1, 0)))
                        }
                      >
                        −
                      </button>
                      <b>{t.est_pomodoros ?? 0}🍅</b>
                      <button title="плюс помидорка к оценке" onClick={() => void run(() => api.editPlanTask(t.id, null, (t.est_pomodoros ?? 0) + 1))}>
                        +
                      </button>
                    </span>
                    <button title={`перенести в «${PLAN_SECTION_LABEL[otherSection(section)]}»`} onClick={() => void run(() => api.editPlanTask(t.id, otherSection(section), null))}>
                      ⇄
                    </button>
                    <button title="убрать из плана (строка «Сегодня» не трогается)" onClick={() => void run(() => api.removePlanTask(t.id))}>
                      ✕
                    </button>
                  </div>
                ))}
              </div>
            </section>
          );
        })}

      {plan && (
        <section className="card">
          <h3>🕐 Встречи</h3>
          {plan.meetings.length === 0 && <p className="muted">Встреч нет — добавь ниже (тема, время, длительность).</p>}
          <div className="sessions">
            {plan.meetings.map((m) => (
              <div key={m.id} className={`plan-task ${m.happened ? "done" : ""}`}>
                <input
                  type="checkbox"
                  checked={m.happened}
                  title="состоялась"
                  onChange={(e) => void run(() => api.setMeetingHappened(m.id, e.currentTarget.checked))}
                />
                <div className="s-main">
                  <div className="s-title">{m.title}</div>
                  <div className="muted small">
                    {m.starts_at ? fmtTime(m.starts_at) : "время не указано"}
                    {m.duration_min ? ` · ${m.duration_min} мин` : ""}
                  </div>
                </div>
              </div>
            ))}
          </div>
          <div className="row">
            <input
              className="grow"
              placeholder="тема встречи"
              value={mTitle}
              onChange={(e) => setMTitle(e.currentTarget.value)}
            />
            <input className="time" placeholder="ЧЧ:ММ" value={mTime} onChange={(e) => setMTime(e.currentTarget.value)} />
            <input
              className="time"
              type="number"
              min={5}
              placeholder="мин"
              value={mMin}
              onChange={(e) => setMMin(e.currentTarget.value)}
            />
            <button
              disabled={!mTitle.trim()}
              onClick={() => {
                void run(() => api.addMeeting(mTitle, mTime.trim() || null, mMin ? Number(mMin) : null));
                setMTitle("");
                setMTime("");
                setMMin("");
              }}
            >
              + встреча
            </button>
          </div>
        </section>
      )}

      {plan && (
        <section className="card">
          <h3>＋ Своя задача</h3>
          <div className="row">
            <select value={newSection} onChange={(e) => setNewSection(e.currentTarget.value as PlanSection)}>
              <option value="work">работа</option>
              <option value="personal">личное</option>
            </select>
            <input
              className="grow"
              placeholder="что сделать"
              value={newTitle}
              onChange={(e) => setNewTitle(e.currentTarget.value)}
            />
            <input
              className="time"
              type="number"
              min={1}
              placeholder="🍅"
              value={newEst}
              onChange={(e) => setNewEst(e.currentTarget.value)}
            />
            <button
              disabled={!newTitle.trim()}
              onClick={() => {
                void run(() =>
                  api.addPlanTask(newSection, newTitle, newEst ? Number(newEst) : null),
                );
                setNewTitle("");
                setNewEst("");
              }}
            >
              + добавить
            </button>
          </div>
        </section>
      )}
    </>
  );
}

// ---------------------------------------------------------------------------
// Парковка
// ---------------------------------------------------------------------------

function ParkingTab({ snap, run }: { snap: Snapshot; run: (fn: (gapReason?: string, asRest?: boolean) => Promise<Snapshot>) => Promise<void> }) {
  const [text, setText] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);
  useEffect(() => inputRef.current?.focus(), []);

  const unresolved = snap.thoughts.filter((t) => !t.resolved);
  const resolved = [...snap.thoughts.filter((t) => t.resolved)].reverse();

  return (
    <section className="card">
      <h3>Парковка мыслей (Cmd+Shift+Space из любого приложения)</h3>
      <div className="row">
        <input
          ref={inputRef}
          className="grow"
          placeholder="мысль, которая лезет в голову посреди фокуса"
          value={text}
          onChange={(e) => setText(e.currentTarget.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && text.trim()) {
              void run(() => api.parkThought(text));
              setText("");
            }
          }}
        />
        <button
          className="primary"
          disabled={!text.trim()}
          onClick={() => {
            void run(() => api.parkThought(text));
            setText("");
          }}
        >
          + припарковать
        </button>
      </div>

      <h4>Ждут разбора ({unresolved.length})</h4>
      {unresolved.length === 0 && <p className="muted">Чисто. Ничего не висит.</p>}
      <div className="thoughts">
        {unresolved.map((t) => (
          <div key={t.id} className="thought">
            <span className="t-text">{t.text}</span>
            <span className="t-actions">
              <button onClick={() => void run(() => api.resolveThought(t.id, "note"))}>
                {RESOLUTION_LABEL.note}
              </button>
              <button onClick={() => void run(() => api.resolveThought(t.id, "task_draft"))}>
                {RESOLUTION_LABEL.task_draft}
              </button>
              <button onClick={() => void run(() => api.resolveThought(t.id, "dismissed"))}>убрать</button>
            </span>
          </div>
        ))}
      </div>

      {resolved.length > 0 && (
        <details>
          <summary>Разобрано ({resolved.length})</summary>
          <div className="thoughts">
            {resolved.map((t) => (
              <div key={t.id} className="thought resolved">
                <span className="t-text">
                  {t.text} <i className="muted">— {t.resolution && RESOLUTION_LABEL[t.resolution]}</i>
                </span>
              </div>
            ))}
          </div>
        </details>
      )}
    </section>
  );
}

// ---------------------------------------------------------------------------
// Настройки
// ---------------------------------------------------------------------------

function SettingsTab() {
  const [settings, setSettings] = useState<Settings | null>(null);
  const [saved, setSaved] = useState(false);
  useEffect(() => {
    void api.getSettings().then(setSettings).catch(String);
  }, []);

  if (!settings) return <p className="muted">Загрузка…</p>;
  const t = settings.timer;
  const num = (k: keyof Settings["timer"], label: string) => (
    <label className="setting">
      <span>{label}</span>
      <input
        type="number"
        min={1}
        value={t[k]}
        onChange={(e) => {
          const v = Number(e.currentTarget.value) || 1;
          setSettings({ ...settings, timer: { ...t, [k]: v } });
          setSaved(false);
        }}
      />
    </label>
  );

  return (
    <section className="card">
      <h3>Таймер</h3>
      <p className="muted">Классика 25/5/15, длинный после 4 помидорок. Меняется только здесь.</p>
      <div className="grid">
        {num("focus_min", "фокус, мин")}
        {num("short_break_min", "короткий перерыв, мин")}
        {num("long_break_min", "длинный перерыв, мин")}
        {num("long_every", "длинный после N помидорок")}
        {num("gap_threshold_min", "порог разрыва, мин")}
      </div>
      <label className="check">
        <input
          type="checkbox"
          checked={settings.autostart}
          onChange={(e) => {
            setSettings({ ...settings, autostart: e.currentTarget.checked });
            setSaved(false);
          }}
        />
        запускать вместе с macOS
      </label>
      <div className="row">
        <button
          className="primary"
          onClick={() => void api.saveSettings(t, settings.autostart).then((s) => {
            setSettings(s);
            setSaved(true);
          })}
        >
          Сохранить
        </button>
        {saved && <span className="muted">сохранено</span>}
      </div>
      <p className="muted">
        БД: {settings.db_online ? "онлайн" : "офлайн — события копятся в ~/.timeshift/wal и дореплицируются"}.
        Секреты — в ~/.timeshift/config.toml, в приложение не встроены.
      </p>
    </section>
  );
}
