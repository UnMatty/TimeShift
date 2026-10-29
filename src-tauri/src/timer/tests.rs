//! Юнит-тесты Timer Core (WP-2): цикл дня, восстановление после перезапуска,
//! банк перерывов, разрывы, ручная до-логировка.

use super::*;
use crate::wal::event::Event;
use chrono::TimeZone;

fn ts_at(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Local> {
    Local.with_ymd_and_hms(y, m, d, h, min, 0).single().expect("валидное локальное время")
}

fn timer() -> Timer {
    Timer::new(TimerConfig::default())
}

/// Прогоняет события через fold со штампом времени — как это делает команда,
/// дописавшая их в WAL. События вычисляются до заёма `&mut`.
fn apply(t: &mut Timer, ts: DateTime<Local>, events: Vec<Event>) {
    for event in events {
        t.fold(&WalRecord { id: Uuid::new_v4(), ts, event });
    }
}

/// Полный цикл: фокус с паузой → завершение → короткий перерыв → следующая
/// помидорка → пропуск перерыва в банк.
#[test]
fn full_day_cycle() {
    let mut t = timer();
    let base = ts_at(2026, 10, 27, 9, 0);

    let events = t.op_start_focus(base, Some("отчёты".into()), false, None, false, None, None).unwrap();
    apply(&mut t, base, events);
    assert!(matches!(t.phase, Phase::Focus(_)));

    // Пауза на 3 минуты с причиной «отвлекли».
    let paused_at = base + chrono::Duration::minutes(10);
    let events = t.op_pause_focus().unwrap();
    apply(&mut t, paused_at, events);
    let resumed_at = paused_at + chrono::Duration::minutes(3);
    let events = t.op_resume_focus(PauseReason::External).unwrap();
    apply(&mut t, resumed_at, events);

    // Естественное завершение по тику: накоплено 10 + 12 = 22 мин, до 25 не хватает.
    let tick_at = resumed_at + chrono::Duration::minutes(12);
    assert!(t.tick(tick_at).is_empty(), "22 мин — ещё не конец");
    let done_at = resumed_at + chrono::Duration::minutes(15);
    let events = t.tick(done_at);
    assert_eq!(events.len(), 1, "25 мин накоплено — автозавершение");
    apply(&mut t, done_at, events);

    assert_eq!(t.streak, 1);
    let s = t.sessions.first().expect("сессия");
    assert_eq!(s.status, SessionStatus::Completed);
    assert_eq!(s.actual_min, Some(25));
    assert_eq!(s.pauses.len(), 1);
    assert_eq!(s.pauses[0].reason, Some(PauseReason::External));
    assert!(matches!(t.phase, Phase::Idle));

    // Предложен короткий перерыв 5 мин; берём.
    let proposed = t.proposed_break().expect("перерыв предложен");
    assert_eq!(proposed.kind, BreakKind::Short);
    let events = t.op_start_break(done_at, None, false).unwrap();
    apply(&mut t, done_at, events);
    let Phase::Break(b) = &t.phase else { panic!("должен быть перерыв") };
    assert_eq!(b.duration_min, 5);

    // Вторая помидорка, после неё скип короткого в длинный: банк = 5.
    t.op_end_break(done_at).expect("закончить перерыв");
    let f2 = done_at + chrono::Duration::minutes(1);
    let events = t.op_start_focus(f2, None, false, None, false, None, None).unwrap();
    apply(&mut t, f2, events);
    let done2 = f2 + chrono::Duration::minutes(25);
    let events = t.tick(done2);
    apply(&mut t, done2, events);
    let events = t.op_skip_break(BreakAction::SkipToLong).unwrap();
    apply(&mut t, done2, events);
    assert_eq!(t.bank_min(), 5);
    assert_eq!(t.streak, 2);

    // Разрыв > порога при старте третьей: сначала вопрос, потом лог.
    let later = done2 + chrono::Duration::minutes(30);
    assert!(matches!(t.op_start_focus(later, None, false, None, false, None, None), Err(OpError::NeedGap { minutes: 30 })));
    let events = t.op_start_focus(later, None, false, Some("обед".into()), false, None, None).unwrap();
    apply(&mut t, later, events);
    assert_eq!(t.gaps.len(), 1);
    assert_eq!(t.gaps[0].reason, "обед");
}

const SID: Uuid = Uuid::from_u128(0x1111);
const PID: Uuid = Uuid::from_u128(0x2222);

fn base_session() -> Event {
    SessionStarted {
        session_id: SID,
        planned_min: 25,
        source: SessionSource::Timer,
        label: None,
        confidential: false,
        plan_task_id: None,
    }
    .into()
}

/// «Убийство процесса» посреди фокуса: свежий таймер поверх тех же записей
/// восстанавливает идущую сессию с учётом отрезков до пауз.
#[test]
fn restart_restores_running_focus() {
    let base = ts_at(2026, 10, 27, 11, 0);

    // Записи журнала, как если бы процесс писал и умер после resume:
    // старт 11:00 → пауза 11:08 → причина 11:10 → resume 11:11.
    let records: Vec<WalRecord> = [
        (base, base_session()),
        (base + chrono::Duration::minutes(8), Event::SessionPaused(SessionPaused { session_id: SID, pause_id: PID })),
        (base + chrono::Duration::minutes(10), Event::PauseReason(PauseReasonEvent { session_id: SID, pause_id: PID, reason: PauseReason::Away })),
        (base + chrono::Duration::minutes(11), Event::SessionResumed(SessionResumed { session_id: SID })),
    ]
    .into_iter()
    .map(|(ts, event)| WalRecord { id: Uuid::new_v4(), ts, event })
    .collect();

    // «Перезапуск»: свежий таймер проигрывает тот же журнал.
    let mut second = timer();
    for r in &records {
        second.fold(r);
    }

    let Phase::Focus(f) = &second.phase else { panic!("фокус должен восстановиться") };
    assert_eq!(f.session_id, SID);
    assert_eq!(f.focus_secs, 8 * 60, "до паузы накоплено 8 мин");
    assert_eq!(f.seg_start, Some(base + chrono::Duration::minutes(11)));
    // Время, пока процесс был мёртв, идёт в счёт: через 14 мин после resume — 8+14=22.
    let snap = second.snapshot(base + chrono::Duration::minutes(25));
    let PhaseView::Focus { elapsed_secs, paused, .. } = snap.phase else { panic!("снапшот фокуса") };
    assert_eq!(elapsed_secs, 22 * 60);
    assert!(!paused);
    assert_eq!(second.sessions[0].status, SessionStatus::Running);
    assert_eq!(second.sessions[0].pauses.len(), 1);
    assert_eq!(second.sessions[0].pauses[0].reason, Some(PauseReason::Away));
}

/// Банк: скип длинного в короткий накапливает; взятый перерыв потребляет банк;
/// закрытие дня обнуляет стрик.
#[test]
fn skip_bank_accumulates_burns_on_day_close() {
    let base = ts_at(2026, 10, 27, 9, 0);
    let mut t = timer();

    // Четыре помидорки по 25 мин с короткими перерывами — до стрика 4.
    let mut now = base;
    for i in 0..4 {
        let events = t.op_start_focus(now, None, false, None, false, None, None).unwrap();
        apply(&mut t, now, events);
        now += chrono::Duration::minutes(25);
        let events = t.tick(now);
        apply(&mut t, now, events);
        if i < 3 {
            let events = t.op_start_break(now, None, false).unwrap();
            apply(&mut t, now, events);
            now += chrono::Duration::minutes(5);
            t.op_end_break(now).expect("закончить перерыв");
        }
    }
    assert_eq!(t.streak, 4);

    // После 4-й предложен длинный; скипаем его В короткий: банк += 15.
    let proposed = t.proposed_break().unwrap();
    assert_eq!(proposed.kind, BreakKind::Long);
    let events = t.op_skip_break(BreakAction::SkipToShort).unwrap();
    apply(&mut t, now, events);
    assert_eq!(t.bank_min(), 15);

    // Ещё помидорка (через 5 минут — не попадая в порог разрыва);
    // предложен короткий 5+15=20; берём — банк потреблён.
    now += chrono::Duration::minutes(5);
    let events = t.op_start_focus(now, None, false, None, false, None, None).unwrap();
    apply(&mut t, now, events);
    now += chrono::Duration::minutes(25);
    let events = t.tick(now);
    apply(&mut t, now, events);
    let proposed = t.proposed_break().unwrap();
    assert_eq!(proposed.kind, BreakKind::Short);
    assert_eq!(proposed.duration_min, 20, "5 базовых + 15 из банка");
    let events = t.op_start_break(now, None, false).unwrap();
    apply(&mut t, now, events);
    assert_eq!(t.bank_min(), 0);

    // Закрытие дня: можно только из Idle; после — стрик обнулён.
    assert!(t.op_close_day().is_err(), "закрывать можно только из Idle");
    now += chrono::Duration::minutes(20);
    t.op_end_break(now).expect("закончить перерыв");
    let events = t.op_close_day().unwrap();
    apply(&mut t, now, events);
    assert_eq!(t.streak, 0);
}

/// Скип «просто пропустить» ничего не копит; сгорание банка при закрытии дня.
#[test]
fn plain_skip_and_burn() {
    let base = ts_at(2026, 10, 27, 9, 0);
    let mut t = timer();
    let events = t.op_start_focus(base, None, false, None, false, None, None).unwrap();
    apply(&mut t, base, events);
    let done = base + chrono::Duration::minutes(25);
    let events = t.tick(done);
    apply(&mut t, done, events);
    let events = t.op_skip_break(BreakAction::SkipPlain).unwrap();
    apply(&mut t, done, events);
    assert_eq!(t.bank_min(), 0);

    // Скопим банк и закроем день.
    let events = t.op_start_focus(done, None, false, None, false, None, None).unwrap();
    apply(&mut t, done, events);
    let done2 = done + chrono::Duration::minutes(25);
    let events = t.tick(done2);
    apply(&mut t, done2, events);
    let events = t.op_skip_break(BreakAction::SkipToShort).unwrap();
    apply(&mut t, done2, events);
    assert_eq!(t.bank_min(), 5);
    let events = t.op_close_day().unwrap();
    let Event::DayClosed(d) = &events[0] else { panic!() };
    assert_eq!(d.burned_min, 5);
    apply(&mut t, done2, events);
    assert_eq!(t.bank_min(), 0);
}

/// Аннулирование не идёт в стрик; ручная сессия, заметка и категория применяются.
#[test]
fn void_manual_note_category() {
    let base = ts_at(2026, 10, 27, 14, 0);
    let mut t = timer();

    let events = t.op_start_focus(base, Some("мусорная".into()), false, None, false, None, None).unwrap();
    apply(&mut t, base, events);
    let voided = base + chrono::Duration::minutes(5);
    let events = t.op_void_focus(Some("сгорела: позвонили".into())).unwrap();
    apply(&mut t, voided, events);
    assert_eq!(t.streak, 0);
    assert_eq!(t.sessions[0].status, SessionStatus::Void);

    let sid = t.sessions[0].id;
    let events = t.op_set_note(sid, "всё равно набросал план".into()).unwrap();
    apply(&mut t, voided, events);
    let events = t.op_set_category(sid, Category::Work).unwrap();
    apply(&mut t, voided, events);
    assert_eq!(t.sessions[0].note, "всё равно набросал план");
    assert_eq!(t.sessions[0].category, Some(Category::Work));

    let manual_start = ts_at(2026, 10, 27, 12, 30);
    let events = t
        .op_add_manual_session(manual_start, 40, Some("блог".into()), None, Some(Category::Personal))
        .unwrap();
    apply(&mut t, voided, events);
    let manual = &t.sessions[1];
    assert_eq!(manual.source, SessionSource::Manual);
    assert_eq!(manual.actual_min, Some(40));
    assert_eq!(manual.label.as_deref(), Some("блог"));
    assert_eq!(manual.category, Some(Category::Personal));
    assert!(matches!(t.phase, Phase::Idle), "ручная сессия не трогает фазу");
}

/// Смена суток по тику: банк фиксируется как сгоревший, стрик обнуляется.
#[test]
fn day_rollover_burns_bank() {
    let base = ts_at(2026, 10, 27, 23, 50);
    let mut t = timer();
    // Таймер создаётся с реальной сегодняшней датой — тесты кочуют по дням,
    // поэтому фиксируем дату начала вручную.
    t.current_date = base.date_naive();
    let events = t.op_start_focus(base, None, false, None, false, None, None).unwrap();
    apply(&mut t, base, events);
    let done = base + chrono::Duration::minutes(9); // 23:59, отработано только 9 мин
    let events = t.op_complete_focus(done).unwrap();
    apply(&mut t, done, events);
    assert_eq!(t.sessions[0].status, SessionStatus::Partial, "9 мин из 25 — частично");
    let events = t.op_skip_break(BreakAction::SkipToLong).unwrap();
    apply(&mut t, done, events);
    assert_eq!(t.bank_min(), 5);

    let midnight = ts_at(2026, 10, 28, 0, 0);
    let events = t.tick_day_rollover(midnight);
    assert_eq!(events.len(), 1, "банк не пуст — должно быть событие");
    apply(&mut t, midnight, events);
    assert_eq!(t.bank_min(), 0);
    assert_eq!(t.streak, 0);
    assert_eq!(t.current_date, midnight.date_naive());

    // Повторный тик в тот же день — ничего.
    assert!(t.tick_day_rollover(midnight + chrono::Duration::minutes(1)).is_empty());
}

/// Смена суток: дневные данные — сессии, разрывы, перерывы — в новый день
/// не переносятся (и по событию сжигания банка, и по прямому пути).
#[test]
fn day_rollover_clears_daily_data() {
    // Путь через событие: банк не пуст — DayClosed, дата двигается в fold.
    let base = ts_at(2026, 10, 27, 23, 20);
    let mut t = timer();
    t.current_date = base.date_naive();
    let events = t.op_start_focus(base, None, false, None, false, None, None).unwrap();
    apply(&mut t, base, events);
    let done = base + chrono::Duration::minutes(25);
    let events = t.tick(done);
    apply(&mut t, done, events);
    let events = t.op_skip_break(BreakAction::SkipToShort).unwrap();
    apply(&mut t, done, events);
    assert_eq!(t.sessions.len(), 1);
    assert!(!t.breaks.is_empty());

    let midnight = ts_at(2026, 10, 28, 0, 0);
    let events = t.tick_day_rollover(midnight);
    assert_eq!(events.len(), 1);
    apply(&mut t, midnight, events);
    assert!(t.sessions.is_empty(), "вчерашние сессии в новый день не переносятся");
    assert!(t.gaps.is_empty());
    assert!(t.breaks.is_empty());

    // Прямой путь: банк и стрик пусты — без события, сброс тот же.
    let evening = ts_at(2026, 10, 28, 22, 0);
    let events = t
        .op_add_manual_session(evening, 40, Some("блог".into()), None, None)
        .unwrap();
    apply(&mut t, evening, events);
    assert_eq!(t.sessions.len(), 1);
    let midnight2 = ts_at(2026, 10, 29, 0, 0);
    assert!(
        t.tick_day_rollover(midnight2).is_empty(),
        "банк и стрик пусты — событие не нужно"
    );
    assert!(t.sessions.is_empty());
    assert_eq!(t.current_date, midnight2.date_naive());
}

/// «Отдохнуть весь банк» (v0.1.1): перерыв длительностью в банк, банк обнулён;
/// при пустом банке операция запрещена.
#[test]
fn bank_break_spends_whole_bank() {
    let base = ts_at(2026, 10, 28, 9, 0);
    let mut t = timer();
    assert!(t.op_start_bank_break(base, None, false).is_err(), "пустой банк");

    // Помидорка + скип короткого в банк = 5.
    let events = t.op_start_focus(base, None, false, None, false, None, None).unwrap();
    apply(&mut t, base, events);
    let done = base + chrono::Duration::minutes(25);
    let events = t.tick(done);
    apply(&mut t, done, events);
    let events = t.op_skip_break(BreakAction::SkipToShort).unwrap();
    apply(&mut t, done, events);
    assert_eq!(t.bank_min(), 5);

    // Ещё помидорка + скип: банк 10; берём весь.
    let f2 = done + chrono::Duration::minutes(5);
    let events = t.op_start_focus(f2, None, false, None, false, None, None).unwrap();
    apply(&mut t, f2, events);
    let done2 = f2 + chrono::Duration::minutes(25);
    let events = t.tick(done2);
    apply(&mut t, done2, events);
    let events = t.op_skip_break(BreakAction::SkipToShort).unwrap();
    apply(&mut t, done2, events);
    assert_eq!(t.bank_min(), 10);

    let events = t.op_start_bank_break(done2, None, false).unwrap();
    let Event::BreakEvent(b) = &events[0] else { panic!() };
    assert_eq!(b.kind, BreakKind::Bank);
    assert_eq!(b.duration_min, 10);
    apply(&mut t, done2, events);
    assert_eq!(t.bank_min(), 0);
    let Phase::Break(bp) = &t.phase else { panic!("должен идти перерыв") };
    assert_eq!(bp.duration_min, 10);
}

/// «Это был отдых» (v0.1.1): минуты разрыва списываются из банка, остаток
/// сверх банка — обычный разрыв; при пустом банке — просто разрыв.
#[test]
fn gap_as_rest_spends_bank_and_logs_remainder() {
    let base = ts_at(2026, 10, 28, 9, 0);
    let mut t = timer();
    let events = t.op_start_focus(base, None, false, None, false, None, None).unwrap();
    apply(&mut t, base, events);
    let done = base + chrono::Duration::minutes(25);
    let events = t.tick(done);
    apply(&mut t, done, events);
    let events = t.op_skip_break(BreakAction::SkipToShort).unwrap();
    apply(&mut t, done, events);
    assert_eq!(t.bank_min(), 5);

    // Разрыв 30 мин при банке 5: 5 мин — отдых, 25 — обычный разрыв.
    let later = done + chrono::Duration::minutes(30);
    let events = t
        .op_start_focus(later, None, false, Some("обед".into()), true, None, None)
        .unwrap();
    assert_eq!(events.len(), 3, "отдых + разрыв + старт фокуса");
    let Event::BreakEvent(b) = &events[0] else { panic!() };
    assert_eq!(b.spend_min, Some(5));
    assert_eq!(b.duration_min, 5);
    let Event::GapLogged(g) = &events[1] else { panic!() };
    assert_eq!((g.ended_at - g.started_at).num_minutes(), 25);
    apply(&mut t, later, events);

    assert_eq!(t.bank_min(), 0);
    assert_eq!(t.gaps.len(), 1);
    assert!(matches!(t.phase, Phase::Focus(_)));
    assert_eq!(t.breaks.last().map(|b| b.duration_min), Some(5));

    // Пустой банк: «это был отдых» вырождается в обычный разрыв.
    let done2 = later + chrono::Duration::minutes(25);
    let events = t.tick(done2);
    apply(&mut t, done2, events);
    let even_later = done2 + chrono::Duration::minutes(20);
    let events = t.op_start_break(even_later, Some("потерял ритм".into()), true).unwrap();
    apply(&mut t, even_later, events);
    assert_eq!(t.gaps.len(), 2, "без банка отдых не списывается");
}

/// Повторное «Завершить день» запрещено; после смены суток — снова можно.
#[test]
fn day_close_is_once_per_day() {
    let base = ts_at(2026, 10, 28, 9, 0);
    let mut t = timer();
    // Таймер создаётся с реальной сегодняшней датой — фиксируем дату начала.
    t.current_date = base.date_naive();
    let events = t.op_start_focus(base, None, false, None, false, None, None).unwrap();
    apply(&mut t, base, events);
    let done = base + chrono::Duration::minutes(25);
    let events = t.tick(done);
    apply(&mut t, done, events);

    let closed = done + chrono::Duration::minutes(6);
    let events = t.op_close_day().unwrap();
    apply(&mut t, closed, events);
    assert!(t.day_closed);
    assert!(t.snapshot(closed).day_closed);
    assert!(t.op_close_day().is_err(), "день уже закрыт");

    // Полночь: день закрыт и банк пуст — rollover просто открывает новый день.
    let midnight = ts_at(2026, 10, 29, 0, 0);
    assert!(t.tick_day_rollover(midnight).is_empty(), "событие не нужно");
    assert!(!t.day_closed, "новый день открыт");

    // Помидорка нового дня: через ночь — большой разрыв, логируем причиной.
    let next = ts_at(2026, 10, 29, 9, 0);
    let events = t.op_start_focus(next, None, false, Some("ночь".into()), false, None, None).unwrap();
    apply(&mut t, next, events);
    let s = t.snapshot(next);
    assert!(!s.day_closed);

    // Помидорка завершается — и день снова можно закрыть.
    let done3 = next + chrono::Duration::minutes(25);
    let events = t.tick(done3);
    apply(&mut t, done3, events);
    assert!(t.op_close_day().is_ok(), "новый день закрывается");

    // Событие третьего дня при проигрывании журнала тоже сбрасывает флаг.
    let third = ts_at(2026, 10, 30, 12, 0);
    apply(
        &mut t,
        third,
        vec![ThoughtParked { thought_id: Uuid::new_v4(), text: "мысль".into() }.into()],
    );
    assert!(!t.day_closed, "fold сбрасывает закрытие при смене суток");
}

/// Категория, выбранная при старте, попадает в сессию сразу.
#[test]
fn category_at_start() {
    let base = ts_at(2026, 10, 28, 9, 0);
    let mut t = timer();
    let events = t
        .op_start_focus(base, Some("созвон".into()), false, None, false, Some(Category::Meeting), None)
        .unwrap();
    assert!(events.iter().any(|e| matches!(e, Event::CategorySet(_))), "категория — событием");
    apply(&mut t, base, events);
    assert_eq!(t.sessions[0].category, Some(Category::Meeting));
}

/// План дня (v0.2): сборка из «Сегодня», раскладка/оценки, отметка → синк,
/// старт помидорки по задаче с фактом в снапшоте (FR-13…FR-18).
#[test]
fn plan_lifecycle() {
    use crate::wal::event::{PlanSection, PlanTask};
    let base = ts_at(2026, 10, 28, 9, 0);
    let mut t = timer();
    // Таймер создаётся с реальной сегодняшней датой — фиксируем дату начала.
    t.current_date = base.date_naive();
    let day = base.date_naive();

    // До сборки плана операции запрещены.
    assert!(t.op_add_plan_task(PlanSection::Work, "своя".into(), None).is_err());

    // Сборка: две задачи из «Сегодня».
    let mk = |section: PlanSection, title: &str, marker: Option<&str>, line: Option<&str>, est: Option<u32>| PlanTask {
        id: Uuid::new_v4(),
        section,
        title: title.to_string(),
        wiki_links: vec!["Ссылка".into()],
        marker: marker.map(str::to_string),
        today_note_line: line.map(str::to_string),
        est_pomodoros: est,
        done: false,
        today_note_done: false,
    };
    let work = mk(PlanSection::Work, "Разобрать баги дашборда", Some("▲"), Some("- [ ] ▲ Разобрать баги дашборда"), Some(2));
    let personal = mk(PlanSection::Personal, "Александрия: претензия", Some("⏫"), None, None);
    let events = t
        .op_build_plan(day, vec![work.clone(), personal.clone()], Some("сырой текст".into()))
        .unwrap();
    apply(&mut t, base, events);

    // Раскладка и оценка правятся.
    let events = t.op_edit_plan_task(personal.id, Some(PlanSection::Work), Some(1)).unwrap();
    apply(&mut t, base, events);
    assert_eq!(t.plan.as_ref().unwrap().tasks[1].section, PlanSection::Work);

    // Старт по задаче: сессия привязана; факт считается по завершённой.
    let events = t
        .op_start_focus(base, Some("Разобрать баги дашборда".into()), false, None, false, None, Some(work.id))
        .unwrap();
    apply(&mut t, base, events);
    assert_eq!(t.sessions[0].plan_task_id, Some(work.id));
    let done = base + chrono::Duration::minutes(25);
    let events = t.tick(done);
    apply(&mut t, done, events);

    let snap = t.snapshot(done);
    let plan = snap.plan.expect("план в снапшоте");
    assert_eq!(plan.tasks[0].fact_pomodoros, 1, "завершённая помидорка по задаче");
    assert!(plan.tasks[0].from_today_note);

    // Отметка выполненной: done меняется, today_note_done — только после синка.
    let events = t.op_set_plan_task_done(work.id, true).unwrap();
    assert!(!events.is_empty());
    apply(&mut t, done, events);
    assert!(t.op_set_plan_task_done(work.id, true).unwrap().is_empty(), "повтор — no-op");
    let events = t.op_plan_task_synced(work.id, true, None).unwrap();
    apply(&mut t, done, events);
    assert!(t.plan.as_ref().unwrap().tasks[0].today_note_done);

    // Конфликт синка состояние не меняет (только аудит в WAL).
    let events = t.op_set_plan_task_done(personal.id, true).unwrap();
    apply(&mut t, done, events);
    let events = t
        .op_plan_task_synced(personal.id, true, Some("строка не найдена".into()))
        .unwrap();
    apply(&mut t, done, events);
    assert!(!t.plan.as_ref().unwrap().tasks[1].today_note_done, "конфликт — чекбокс не подтверждён");

    // Встречи: добавление и вечерняя отметка.
    let events = t.op_add_meeting("Встреча с аналитиками".into(), None, Some(45)).unwrap();
    apply(&mut t, done, events);
    let mid = t.plan.as_ref().unwrap().meetings[0].id;
    let events = t.op_set_meeting_happened(mid, true).unwrap();
    apply(&mut t, done, events);
    assert!(t.snapshot(done).plan.unwrap().meetings[0].happened);

    // Пересборка хранит встречи, заменяет задачи.
    let fresh = mk(PlanSection::Work, "Свежая задача", None, None, None);
    let events = t.op_build_plan(day, vec![fresh], None).unwrap();
    apply(&mut t, done, events);
    let plan = t.plan.as_ref().unwrap();
    assert_eq!(plan.tasks.len(), 1);
    assert_eq!(plan.meetings.len(), 1, "встречи переживают пересборку");
}
