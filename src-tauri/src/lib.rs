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
use wal::event::{BreakAction, PauseReason};
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
            commands::start_focus(app.clone(), None, false, None, false, None).ok();
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
