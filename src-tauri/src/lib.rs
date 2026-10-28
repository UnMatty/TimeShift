//! TimeShift — точка сборки (WP-3): плагины, трей, тик-цикл, репликатор БД,
//! глобальный хоткей парковки. Поведение «резидент»: закрытие окна прячет его,
//! выход — из меню трея.

pub mod commands;
pub mod config;
pub mod db;
pub mod timer;
pub mod vault;
pub mod wal;

use std::time::Duration;

use chrono::Local;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, WindowEvent};
use tauri_plugin_autostart::MacosLauncher;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

use commands::AppState;
use config::Config;
use timer::Timer;
use uuid::Uuid;
use vault::client::VaultError;
use wal::event::{BreakAction, PauseReason, PlanTaskSynced};
use wal::Wal;

const PARK_SHORTCUT: &str = "CmdOrCtrl+Shift+Space";

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let config = Config::load();

    // Восстановление состояния дня из журнала: убийство процесса не теряет время.
    let mut timer = Timer::new(config.timer);
    let wal = Wal::open_default().expect("каталог журнала ~/.timeshift/wal");
    match wal::read_all(wal.dir()) {
        Ok(playback) => {
            if playback.skipped_lines > 0 || playback.skipped_files > 0 {
                eprintln!(
                    "timeshift: в журнале пропущено строк: {}, посторонних файлов: {}",
                    playback.skipped_lines, playback.skipped_files
                );
            }
            for record in &playback.events {
                timer.fold(record);
            }
            println!("timeshift: журнал проигран, событий: {}", playback.events.len());
        }
        Err(e) => eprintln!("timeshift: журнал не прочитан ({e}); старт с пустого состояния"),
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        open_parking(app);
                    }
                })
                .build(),
        )
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None))
        .manage(AppState::new(timer, wal, config))
        .invoke_handler(tauri::generate_handler![
            commands::get_snapshot,
            commands::db_status,
            commands::start_focus,
            commands::pause_focus,
            commands::resume_focus,
            commands::complete_focus,
            commands::void_session,
            commands::start_break,
            commands::start_bank_break,
            commands::skip_break,
            commands::end_break,
            commands::park_thought,
            commands::resolve_thought,
            commands::set_note,
            commands::set_category,
            commands::add_manual_session,
            commands::close_day,
            commands::build_plan_from_today,
            commands::add_plan_task,
            commands::remove_plan_task,
            commands::edit_plan_task,
            commands::set_plan_task_done,
            commands::add_meeting,
            commands::set_meeting_happened,
            commands::get_settings,
            commands::save_settings,
        ])
        .on_window_event(|window, event| {
            // Крестик прячет окно — приложение продолжает жить в меню-баре.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                window.hide().ok();
            }
        })
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            build_tray(app.handle())?;

            // Глобальный хоткей парковки (FR-10).
            app.global_shortcut()
                .register(PARK_SHORTCUT)
                .map_err(|e| format!("не удалось зарегистрировать хоткей {PARK_SHORTCUT}: {e}"))?;

            // Тик: автозавершение этапов, полночь, трей, события в UI.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move { tick_loop(handle).await });

            // Репликатор WAL → Postgres (WP-4).
            let (url, wal_dir, online) = {
                let state = app.state::<AppState>();
                let cfg = state.config.lock().expect("конфиг");
                let wal = state.wal.lock().expect("журнал");
                (cfg.db.url.clone(), wal.dir().to_path_buf(), state.db_online_flag())
            };
            tauri::async_runtime::spawn(async move {
                db::replicator_loop(url, wal_dir, online).await;
            });

            // Vault-синк (v0.2): чекбоксы «☀️ Сегодня» + фокус-лог при закрытии дня.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move { vault_loop(handle).await });

            // Восстановить автозапуск из конфига.
            if app.state::<AppState>().config.lock().expect("конфиг").app.autostart {
                use tauri_plugin_autostart::ManagerExt as _;
                app.autolaunch().enable().ok();
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("ошибка запуска TimeShift");
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let item = |id: &str, text: &str| -> tauri::Result<MenuItem<tauri::Wry>> {
        MenuItem::with_id(app, id, text, true, None::<&str>)
    };

    let start = item("start_focus", "🍅 Старт фокуса")?;
    let pause = item("pause", "⏸ Пауза")?;
    let resume = item("resume", "▶️ Продолжить")?;
    let complete = item("complete", "✅ Завершить помидорку")?;
    let void_item = item("void", "🚫 Аннулировать помидорку")?;
    let start_break = item("start_break", "☕ Старт перерыва")?;
    let bank_break = item("bank_break", "🛌 Отдохнуть весь банк")?;
    let end_break = item("end_break", "⏹ Закончить перерыв")?;
    let skip_short = item("skip_short", "прибавить к следующему короткому")?;
    let skip_long = item("skip_long", "прибавить к следующему длинному")?;
    let skip_plain = item("skip_plain", "просто пропустить")?;
    let skip = Submenu::with_items(app, "⏭ Пропустить перерыв", true, &[&skip_short, &skip_long, &skip_plain])?;
    let park = item("park", "💭 Мысль на парковку")?;
    let close_day = item("close_day", "🌙 Завершить день")?;
    let open = item("open", "Открыть TimeShift")?;
    let quit = item("quit", "Выход")?;

    let menu = Menu::with_items(
        app,
        &[
            &start,
            &pause,
            &resume,
            &complete,
            &void_item,
            &PredefinedMenuItem::separator(app)?,
            &start_break,
            &bank_break,
            &end_break,
            &skip,
            &PredefinedMenuItem::separator(app)?,
            &park,
            &close_day,
            &PredefinedMenuItem::separator(app)?,
            &open,
            &quit,
        ],
    )?;

    let tray = TrayIconBuilder::with_id("main-tray")
        .icon(app.default_window_icon().expect("иконка приложения").clone())
        .menu(&menu)
        .show_menu_on_left_click(true)
        .title("◦")
        .on_menu_event(|app, event| on_tray_event(app, event.id().as_ref()))
        .build(app)?;
    let _ = app.state::<AppState>().tray.set(tray);
    Ok(())
}

/// Обработка пунктов меню трея — те же операции, что и команды окна.
fn on_tray_event(app: &AppHandle, id: &str) {
    match id {
        "start_focus" => {
            commands::start_focus(app.clone(), None, false, None, false, None, None).ok();
        }
        "pause" => {
            commands::pause_focus(app.clone()).ok();
        }
        "resume" => {
            // Из трея продолжаем с нейтральной причиной — уточнить можно в окне.
            commands::resume_focus(app.clone(), PauseReason::Internal).ok();
        }
        "complete" => {
            commands::complete_focus(app.clone()).ok();
        }
        "void" => {
            commands::void_session(app.clone(), Some("аннулировано из трея".into())).ok();
        }
        "start_break" => {
            commands::start_break(app.clone(), None, false).ok();
        }
        "bank_break" => {
            commands::start_bank_break(app.clone(), None, false).ok();
        }
        "end_break" => {
            commands::end_break(app.clone()).ok();
        }
        "skip_short" => {
            commands::skip_break(app.clone(), BreakAction::SkipToShort).ok();
        }
        "skip_long" => {
            commands::skip_break(app.clone(), BreakAction::SkipToLong).ok();
        }
        "skip_plain" => {
            commands::skip_break(app.clone(), BreakAction::SkipPlain).ok();
        }
        "park" => open_parking(app),
        "close_day" => {
            commands::close_day(app.clone()).ok();
        }
        "open" => show_main(app),
        "quit" => {
            app.exit(0);
        }
        _ => {}
    }
}

/// Показать основное окно и сфокусировать ввод парковки (FR-10).
fn open_parking(app: &AppHandle) {
    show_main(app);
    app.emit("park-hotkey", ()).ok();
}

/// Vault-синк (v0.2, ARCH §3.4): производный outbox. Obsidian закрыт — молча
/// ждём следующего тика (FR-20), всё остальное работает.
/// 1. Чекбоксы: задачи плана, у которых `done != today_note_done` → точечная
///    замена строки «☀️ Сегодня» (свежее чтение → замена → PUT; строка не
///    нашлась — конфликт, отказ от записи, аудит в WAL).
/// 2. Фокус-лог: день закрыт, заметки ещё нет → генерируем и пишем (FR-21).
async fn vault_loop(app: AppHandle) {
    let mut note_written_for: Option<chrono::NaiveDate> = None;
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        let now = chrono::Local::now();

        // --- 1. Чекбоксы «☀️ Сегодня» -------------------------------------
        let pending: Vec<(Uuid, bool, String)> = {
            let state = app.state::<AppState>();
            let timer = state.timer.lock().expect("таймер");
            match &timer.plan {
                Some(plan) if plan.day == timer.current_date => plan
                    .tasks
                    .iter()
                    .filter(|t| t.done != t.today_note_done)
                    .filter_map(|t| t.today_note_line.clone().map(|line| (t.id, t.done, line)))
                    .collect(),
                _ => Vec::new(),
            }
        };
        if !pending.is_empty()
            && let Ok(client) = commands::vault_client(&app)
            && let Ok(text) = client.read_note(vault::TODAY_NOTE_PATH).await
        {
            let mut updated = text.clone();
            let mut results: Vec<(Uuid, bool, Option<String>)> = Vec::new();
            let mut to_write = false;
            for (task_id, done, line) in pending {
                let target = line.trim_end();
                let replacement = toggle_checkbox(target, done);
                if replacement == target || !updated.contains(target) {
                    results.push((
                        task_id,
                        done,
                        Some("строка не найдена — «☀️ Сегодня» пересобран агентом".into()),
                    ));
                    continue;
                }
                updated = updated.replacen(target, &replacement, 1);
                to_write = true;
                results.push((task_id, done, None));
            }
            let write_ok = !to_write;
            let write_ok = if to_write {
                client.write_note(vault::TODAY_NOTE_PATH, &updated).await.is_ok()
            } else {
                write_ok
            };
            for (task_id, done, conflict) in results {
                let event = match conflict {
                    // Конфликт: состояние не трогаем, в WAL уходит аудит.
                    Some(detail) => PlanTaskSynced { task_id, today_note_done: done, conflict: Some(detail) },
                    // Успешная замена фиксируется только если PUT прошёл;
                    // иначе — тишина, синк повторится в следующем тике.
                    None if write_ok => PlanTaskSynced { task_id, today_note_done: done, conflict: None },
                    None => continue,
                };
                append_and_fold(&app, vec![event.into()]);
            }
        }

        // --- 2. Фокус-лог при закрытии дня (FR-21) --------------------------
        let (closed, day) = {
            let state = app.state::<AppState>();
            let timer = state.timer.lock().expect("таймер");
            (timer.day_closed, timer.current_date)
        };
        if closed
            && note_written_for != Some(day)
            && let Ok(client) = commands::vault_client(&app)
        {
            let path = vault::daily_note::daily_note_path(day);
            match client.read_note(&path).await {
                Ok(_) => note_written_for = Some(day), // уже записан ранее (перезапуск)
                Err(VaultError::NotFound(_)) => {
                    let content = {
                        let state = app.state::<AppState>();
                        let timer = state.timer.lock().expect("таймер");
                        vault::daily_note::generate(&timer.snapshot(now))
                    };
                    if client.write_note(&path, &content).await.is_ok() {
                        note_written_for = Some(day);
                        commands::notify(
                            &app,
                            "🗒 Фокус-лог записан",
                            &format!("{path} — в хранилище, рядом с «☀️ Сегодня»"),
                        );
                    }
                    // недоступно — повторим в следующем тике
                }
                Err(_) => { /* недоступно — повторим в следующем тике */ }
            }
        }
    }
}

/// `- [ ]` ↔ `- [x]` в конкретной строке (обратный синхрон тоже поддержан).
fn toggle_checkbox(line: &str, done: bool) -> String {
    if done {
        line.replacen("- [ ]", "- [x]", 1)
    } else {
        line.replacen("- [x]", "- [ ]", 1)
    }
}

/// События → WAL → fold (без снимка в UI — фоновая операция).
fn append_and_fold(app: &AppHandle, events: Vec<wal::event::Event>) {
    let state = app.state::<AppState>();
    let mut records = Vec::with_capacity(events.len());
    {
        let mut wal = state.wal.lock().expect("журнал");
        for event in events {
            if let Ok(record) = wal.append(event) {
                records.push(record);
            }
        }
    }
    let mut timer = state.timer.lock().expect("таймер");
    for record in &records {
        timer.fold(record);
    }
}

fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        window.show().ok();
        window.set_focus().ok();
    }
}

/// Секундный цикл: автозавершение этапов, смена суток, трей, события в UI.
async fn tick_loop(app: AppHandle) {
    let mut prev_break_running = false;
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let now = Local::now();

        let events = {
            let state = app.state::<AppState>();
            let mut timer = state.timer.lock().expect("таймер");
            let mut events = timer.tick(now);
            events.extend(timer.tick_day_rollover(now));
            events
        };
        if !events.is_empty() {
            let state = app.state::<AppState>();
            let mut records = Vec::with_capacity(events.len());
            {
                let mut wal = state.wal.lock().expect("журнал");
                for event in events {
                    if let Ok(record) = wal.append(event) {
                        records.push(record);
                    }
                }
            }
            {
                let mut timer = state.timer.lock().expect("таймер");
                for record in &records {
                    timer.fold(record);
                }
            }
            commands::notify_side_effects(&app, &records);        }

        let snap = {
            let state = app.state::<AppState>();
            state.timer.lock().expect("таймер").snapshot(now)
        };

        // Окончание перерыва по тику — мягкое уведомление (FR-28).
        let break_running = matches!(snap.phase, timer::PhaseView::Break { .. });        if prev_break_running && !break_running {
            commands::notify_break_over(&app);
        }
        prev_break_running = break_running;

        app.emit("tick", &snap).ok();
        commands::update_tray(&app, &snap);
    }
}
