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

    let events = t.op_start_focus(base, Some("отчёты".into()), false, None).unwrap();
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
    let events = t.op_start_break(done_at, None).unwrap();
    apply(&mut t, done_at, events);
    let Phase::Break(b) = &t.phase else { panic!("должен быть перерыв") };
    assert_eq!(b.duration_min, 5);

    // Вторая помидорка, после неё скип короткого в длинный: банк = 5.
    t.op_end_break(done_at).expect("закончить перерыв");
    let f2 = done_at + chrono::Duration::minutes(1);
    let events = t.op_start_focus(f2, None, false, None).unwrap();
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
    assert!(matches!(t.op_start_focus(later, None, false, None), Err(OpError::NeedGap { minutes: 30 })));
    let events = t.op_start_focus(later, None, false, Some("обед".into())).unwrap();
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
        let events = t.op_start_focus(now, None, false, None).unwrap();
        apply(&mut t, now, events);
        now += chrono::Duration::minutes(25);
        let events = t.tick(now);
        apply(&mut t, now, events);
        if i < 3 {
            let events = t.op_start_break(now, None).unwrap();
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
    let events = t.op_start_focus(now, None, false, None).unwrap();
    apply(&mut t, now, events);
    now += chrono::Duration::minutes(25);
    let events = t.tick(now);
    apply(&mut t, now, events);
    let proposed = t.proposed_break().unwrap();
    assert_eq!(proposed.kind, BreakKind::Short);
    assert_eq!(proposed.duration_min, 20, "5 базовых + 15 из банка");
    let events = t.op_start_break(now, None).unwrap();
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
    let events = t.op_start_focus(base, None, false, None).unwrap();
    apply(&mut t, base, events);
    let done = base + chrono::Duration::minutes(25);
    let events = t.tick(done);
    apply(&mut t, done, events);
    let events = t.op_skip_break(BreakAction::SkipPlain).unwrap();
    apply(&mut t, done, events);
    assert_eq!(t.bank_min(), 0);

    // Скопим банк и закроем день.
    let events = t.op_start_focus(done, None, false, None).unwrap();
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

    let events = t.op_start_focus(base, Some("мусорная".into()), false, None).unwrap();
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
    let events = t.op_start_focus(base, None, false, None).unwrap();
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
