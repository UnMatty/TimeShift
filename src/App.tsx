// TimeShift UI v0.1 (WP-5): экраны «День», «Парковка», «Настройки».
// Все операции идут через api.ts; состояние приходит событием tick из Rust.
import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  api,
  CATEGORY_LABEL,
  isCmdError,
  PAUSE_REASON_LABEL,
  RESOLUTION_LABEL,
  STATUS_LABEL,
  fmtClock,
  fmtTime,
} from "./api";
import type { Category, PauseReason, Settings, Snapshot, UiSession } from "./api";
import "./App.css";

type Tab = "day" | "parking" | "settings";

export default function App() {
  const [tab, setTab] = useState<Tab>("day");
  const [snap, setSnap] = useState<Snapshot | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Вопрос о разрыве (FR-4): ждёт ответа «где был?» перед стартом этапа.
  const [gap, setGap] = useState<{ minutes: number; run: (reason: string) => void } | null>(null);

  // Операция — функция от причины разрыва: при GapNeeded она вызовется повторно.
  const run = useCallback(async (fn: (gapReason?: string) => Promise<Snapshot>) => {
    try {
      setSnap(await fn(undefined));
      setError(null);
    } catch (e) {
      if (isCmdError(e)) {
        if (e.kind === "gap_needed") {
          setGap({
            minutes: e.minutes,
            run: (reason) => {
              setGap(null);
              void run(() => fn(reason));
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

      {gap && <GapDialog minutes={gap.minutes} onAnswer={(r) => gap.run(r)} />}

      {tab === "day" && <DayTab snap={snap} run={run} />}
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

function GapDialog({ minutes, onAnswer }: { minutes: number; onAnswer: (reason: string) => void }) {
  const [reason, setReason] = useState("обед");
  const [other, setOther] = useState("");
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
          <button className="primary" onClick={() => onAnswer(other.trim() || reason)}>
            Записать разрыв
          </button>
        </div>
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
  run: (fn: (gapReason?: string) => Promise<Snapshot>) => Promise<void>;
}) {
  return (
    <>
      <PhaseCard snap={snap} run={run} />
      <SessionsCard snap={snap} run={run} />
      <ManualCard run={run} />
      <footer className="day-footer">
        <button onClick={() => void run(() => api.closeDay())}>🌙 Завершить день (сжечь банк отдыха)</button>
      </footer>
    </>
  );
}

function PhaseCard({ snap, run }: { snap: Snapshot; run: (fn: (gapReason?: string) => Promise<Snapshot>) => Promise<void> }) {
  const [label, setLabel] = useState("");
  const [confidential, setConfidential] = useState(false);
  const [pauseReason, setPauseReason] = useState<PauseReason>("internal");

  const phase = snap.phase;
  const proposed = snap.proposed_break;

  return (
    <section className="card phase-card">
      {phase.kind === "idle" && (
        <div className="phase idle">
          <div className="big">◦</div>
          <div className="phase-info">
            <h2>Свободен</h2>
            <p className="muted">
              Стрик: {snap.streak} 🍅 · Банк отдыха: {snap.bank_min} мин
              {snap.gaps.length > 0 && <> · Разрывов записано: {snap.gaps.length}</>}
            </p>
            <div className="row">
              <input
                className="grow"
                placeholder="над чем работаешь (необязательно)"
                value={label}
                onChange={(e) => setLabel(e.currentTarget.value)}
              />
              <label className="check">
                <input
                  type="checkbox"
                  checked={confidential}
                  onChange={(e) => setConfidential(e.currentTarget.checked)}
                />
                конфиденциально
              </label>
              <button
                className="primary"
                onClick={() => void run((gr) => api.startFocus(label || null, confidential, gr ?? null))}
              >
                🍅 Старт фокуса
              </button>
            </div>
            {proposed && (
              <div className="row break-proposal">
                <span>
                  Предложен {proposed.kind === "long" ? "длинный" : "короткий"} перерыв:{" "}
                  <b>{proposed.duration_min} мин</b>
                </span>
                <button onClick={() => void run((gr) => api.startBreak(gr ?? null))}>☕ Взять перерыв</button>
                <button onClick={() => void run(() => api.skipBreak("skip_to_short"))}>
                  ⏭ скип → в короткий
                </button>
                <button onClick={() => void run(() => api.skipBreak("skip_to_long"))}>
                  ⏭ скип → в длинный
                </button>
                <button onClick={() => void run(() => api.skipBreak("skip_plain"))}>⏭ просто скип</button>
              </div>
            )}
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
                <button onClick={() => void run(() => api.completeFocus())}>✅ Завершить сейчас</button>
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
          <div className="big">{phase.break_kind === "long" ? "☕" : "💬"}</div>
          <div className="phase-info">
            <h2>
              {fmtClock(phase.remaining_secs)}
              <span className="muted"> / {phase.duration_min}:00</span>
            </h2>
            <p className="muted">
              {phase.break_kind === "long" ? "Длинный" : "Короткий"} перерыв — отойди от экрана
            </p>
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
  run: (fn: (gapReason?: string) => Promise<Snapshot>) => Promise<void>;
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
  run: (fn: (gapReason?: string) => Promise<Snapshot>) => Promise<void>;
}) {
  const [note, setNote] = useState(s.note);
  useEffect(() => setNote(s.note), [s.note]);
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
        <input
          className="s-note"
          placeholder="что сделано (FR-8)"
          value={note}
          onChange={(e) => setNote(e.currentTarget.value)}
          onBlur={() => {
            if (note !== s.note) void run(() => api.setNote(s.id, note));
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter") (e.currentTarget as HTMLInputElement).blur();
          }}
        />
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
    </div>
  );
}

function ManualCard({ run }: { run: (fn: (gapReason?: string) => Promise<Snapshot>) => Promise<void> }) {
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
// Парковка
// ---------------------------------------------------------------------------

function ParkingTab({ snap, run }: { snap: Snapshot; run: (fn: (gapReason?: string) => Promise<Snapshot>) => Promise<void> }) {
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
