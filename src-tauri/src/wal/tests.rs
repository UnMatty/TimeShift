use std::io::Write;

use chrono::{Local, NaiveDate, TimeZone};
use tempfile::TempDir;
use uuid::Uuid;

use super::*;
use crate::wal::event::*;

fn temp_wal() -> (TempDir, Wal) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let wal = Wal::open(tmp.path()).expect("open wal");
    (tmp, wal)
}

fn ts_at(y: i32, m: u32, d: u32, h: u32, min: u32) -> chrono::DateTime<Local> {
    Local.with_ymd_and_hms(y, m, d, h, min, 0).single().expect("валидное локальное время")
}

/// Формат записи — контракт с Replicator: ровно четыре ключа верхнего уровня,
/// `type`/`payload` разворачиваются из Event.
#[test]
fn record_shape_locks_contract() {
    let (_tmp, mut wal) = temp_wal();
    let session_id = Uuid::new_v4();
    let record = wal
        .append(SessionStarted {
            session_id,
            planned_min: 25,
            source: SessionSource::Timer,
            label: Some("отчёты".into()),
            confidential: false,
           plan_task_id: None,
        })
        .expect("append");

    let content = std::fs::read_to_string(
        wal.dir().join(format!("{}.jsonl", Local::now().date_naive())),
    )
    .expect("read file");
    let value: serde_json::Value = serde_json::from_str(content.trim_end()).expect("json");

    let mut keys: Vec<&str> = value.as_object().expect("object").keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, vec!["id", "payload", "ts", "type"]);
    assert_eq!(value["type"], "session_started");
    assert_eq!(value["payload"]["session_id"], session_id.to_string());
    assert_eq!(value["payload"]["planned_min"], 25);
    // Десериализация возвращает ту же запись (id и штамп времени сохранились).
    let back: WalRecord = serde_json::from_str(content.trim_end()).expect("deserialize");
    assert_eq!(back, record);
}

/// Все варианты событий проходят запись → чтение без потерь.
#[test]
fn roundtrip_all_event_variants() {
    let (_tmp, mut wal) = temp_wal();
    let session_id = Uuid::new_v4();
    let pause_id = Uuid::new_v4();
    let thought_id = Uuid::new_v4();
    let now = Local::now();

    let events: Vec<Event> = vec![
        SessionStarted { session_id, planned_min: 25, source: SessionSource::Timer, label: None, confidential: true, plan_task_id: None }.into(),
        SessionPaused { session_id, pause_id }.into(),
        PauseReasonEvent { session_id, pause_id, reason: PauseReason::External }.into(),
        SessionResumed { session_id }.into(),
        SessionCompleted { session_id, actual_min: 23 }.into(),
        SessionVoided { session_id, reason: Some("сгорела".into()) }.into(),
        GapLogged { started_at: now, ended_at: now, reason: "обед".into() }.into(),
        NoteEdited { session_id, note: "профили созданы".into() }.into(),
        CategorySet { session_id, category: Category::Work }.into(),
        ThoughtParked { thought_id, text: "спросить бюджет".into() }.into(),
        ThoughtResolved { thought_id, resolution: ThoughtResolution::TaskDraft }.into(),
        BreakEvent { kind: BreakKind::Short, action: BreakAction::SkipToLong, banked_min: 5, duration_min: 0, spend_min: None }.into(),
        ManualSessionAdded {
            session_id,
            started_at: now,
            ended_at: now,
            actual_min: 40,
            label: Some("кросспостинг Jada".into()),
            note: None,
            category: Some(Category::Personal),
        }
        .into(),
        DayClosed { burned_min: 10, banked_min_at_close: 10 }.into(),
        PlanBuilt {
            day: Local::now().date_naive(),
            tasks: vec![PlanTask {
                id: Uuid::new_v4(),
                section: PlanSection::Work,
                title: "задача из «Сегодня»".into(),
                wiki_links: vec!["Ссылка".into()],
                marker: Some("▲".into()),
                today_note_line: Some("- [ ] ▲ задача".into()),
                est_pomodoros: Some(2),
                done: true,
                today_note_done: false,
            }],
            today_note_snapshot: Some("сырой текст заметки".into()),
        }
        .into(),
        MeetingAdded {
            meeting: PlanMeeting {
                id: Uuid::new_v4(),
                title: "Дейлик".into(),
                starts_at: Some(now),
                duration_min: Some(15),
                happened: true,
            },
        }
        .into(),
        PlanTaskSynced { task_id: Uuid::new_v4(), today_note_done: true, conflict: None }.into(),
        VaultSyncStatus { ok: false, detail: Some("Docker остановлен".into()) }.into(),
    ];

    for event in &events {
        wal.append(event.clone()).expect("append");
    }
    let playback = read_all(wal.dir()).expect("read_all");
    assert_eq!(playback.events.len(), 18);
    assert_eq!(playback.skipped_lines, 0);
    assert_eq!(playback.skipped_files, 0);
    for (original, played) in events.iter().zip(playback.events.iter()) {
        assert_eq!(&played.event, original);
    }
}

/// Обрыв процесса посреди записи: оборванная последняя строка пропускается,
/// остальные записи читаются.
#[test]
fn torn_tail_is_skipped_not_fatal() {
    let (tmp, mut wal) = temp_wal();
    wal.append(SessionResumed { session_id: Uuid::new_v4() }).expect("append");
    wal.append(SessionResumed { session_id: Uuid::new_v4() }).expect("append");

    // Имитация смерти процесса посреди записи третьей строки.
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(wal.dir().join(format!("{}.jsonl", Local::now().date_naive())))
        .expect("open for append");
    // Оборванная запись: валидное начало JSON-строки без хвоста и без перевода строки.
    file.write_all(b"{\"id\":").expect("write torn");

    let playback = read_all(wal.dir()).expect("read_all must tolerate torn tail");
    assert_eq!(playback.events.len(), 2);
    assert_eq!(playback.skipped_lines, 1);
    let _ = tmp;
}

/// При смене локальной даты запись уходит в новый файл `ГГГГ-ММ-ДД.jsonl`.
#[test]
fn day_rollover_creates_new_file() {
    let (_tmp, mut wal) = temp_wal();
    let d1 = NaiveDate::from_ymd_opt(2026, 10, 26).unwrap();
    let d2 = NaiveDate::from_ymd_opt(2026, 10, 27).unwrap();

    wal.append_on(d1, ts_at(2026, 10, 26, 23, 59), SessionResumed { session_id: Uuid::new_v4() })
        .expect("append d1");
    wal.append_on(d2, ts_at(2026, 10, 27, 0, 1), SessionResumed { session_id: Uuid::new_v4() })
        .expect("append d2");
    // Третья запись с датой d1 снова в первый файл — файл выбирается по дате, не по «последнему».
    wal.append_on(d1, ts_at(2026, 10, 26, 23, 59).checked_add_signed(chrono::Duration::seconds(30)).unwrap(), SessionResumed { session_id: Uuid::new_v4() })
        .expect("append d1 again");

    assert!(wal.dir().join("2026-10-26.jsonl").exists());
    assert!(wal.dir().join("2026-10-27.jsonl").exists());

    let all = read_all(wal.dir()).expect("read_all");
    assert_eq!(all.events.len(), 3);
    // Порядок — по ts, независимо от того, в какой файл запись легла:
    // 23:59:00 → 23:59:30 → 00:01 следующего дня.
    assert_eq!(all.events[0].ts, ts_at(2026, 10, 26, 23, 59));
    assert_eq!(all.events[1].ts, ts_at(2026, 10, 26, 23, 59) + chrono::Duration::seconds(30));
    assert_eq!(all.events[2].ts, ts_at(2026, 10, 27, 0, 1));

    let day1 = read_day(wal.dir(), d1).expect("read_day");
    assert_eq!(day1.events.len(), 2);
    let day2 = read_day(wal.dir(), d2).expect("read_day");
    assert_eq!(day2.events.len(), 1);
}

/// «Перезапуск приложения»: новый Wal поверх того же каталога видит все записи.
#[test]
fn events_survive_restart() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().to_path_buf();

    let mut first = Wal::open(&dir).expect("open");
    let record = first
        .append(DayClosed { burned_min: 7, banked_min_at_close: 7 })
        .expect("append");
    drop(first);

    let playback = read_all(&dir).expect("read_all after restart");
    assert_eq!(playback.events, vec![record]);

    // И повторное открытие продолжает писать в тот же файл.
    let mut second = Wal::open(&dir).expect("reopen");
    second
        .append(DayClosed { burned_min: 0, banked_min_at_close: 0 })
        .expect("append after reopen");
    assert_eq!(read_all(&dir).expect("read_all").events.len(), 2);
}

/// Посторонний файл в каталоге не ломает проигрывание и учитывается отдельно.
#[test]
fn unrecognized_files_are_counted_not_fatal() {
    let (tmp, mut wal) = temp_wal();
    std::fs::write(tmp.path().join("notes.txt"), "не журнал").expect("write stray file");
    wal.append(SessionResumed { session_id: Uuid::new_v4() }).expect("append");

    let playback = read_all(wal.dir()).expect("read_all");
    assert_eq!(playback.events.len(), 1);
    assert_eq!(playback.skipped_files, 1);
}

/// Пустой каталог — пустое проигрывание без ошибок.
#[test]
fn empty_dir_is_empty_playback() {
    let (tmp, _wal) = temp_wal();
    let playback = read_all(tmp.path()).expect("read_all");
    assert_eq!(playback, Playback::default());
}
