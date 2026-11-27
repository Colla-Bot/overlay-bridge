//! `CollaBot` Overlay Bridge: tells the overlay's pages who is speaking in the user's Discord voice
//! channel. A page cannot ask Discord itself, as Discord's client only answers the origins an
//! application has registered with it, so this small program asks in its place and passes on only
//! what the overlay needs.
//!
//! The part that talks to Discord, in `discord.rs`, is a Rust port of the RPC manager of Overlayed
//! (<https://github.com/overlayeddev/overlayed>), whose license the program shares.

// Without this a release build opens a console window on Windows.
#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

mod config;
mod discord;
mod server;
mod state;
mod tokens;

use std::sync::Arc;

use serde::Serialize;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, State, WindowEvent,
};
use tauri_plugin_deep_link::DeepLinkExt;

use crate::{
    state::{Command, Local, Shared, Snapshot},
    tokens::TokenStore,
};

const MAIN_WINDOW: &str = "main";
/// The event that brings the window a new status.
const STATUS_EVENT: &str = "status";

/// What the window shows: what the pages are told, and how the server for them is doing.
#[derive(Clone, Serialize)]
struct Status {
    #[serde(flatten)]
    snapshot: Snapshot,
    #[serde(flatten)]
    local: Local,
}

fn read_status(shared: &Shared) -> Status {
    Status {
        snapshot: shared.snapshot().borrow().clone(),
        local: shared.local().borrow().clone(),
    }
}

#[tauri::command]
fn get_status(shared: State<'_, Arc<Shared>>) -> Status {
    read_status(&shared)
}

#[tauri::command]
fn request_authorization(shared: State<'_, Arc<Shared>>) {
    shared.send(Command::Authorize);
}

#[tauri::command]
fn forget_authorization(shared: State<'_, Arc<Shared>>) {
    shared.send(Command::Forget);
}

fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// Whether the arguments of a launch are those of a link: Windows and Linux start the program
/// that handles a link with the link as its only argument.
fn is_link_launch(arguments: &[String]) -> bool {
    matches!(arguments, [_, link] if link.starts_with(concat!("collabot-overlay", ":")))
}

/// The bridge lives in the tray, as there is nothing to do in its window most of the time.
fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "열기", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "종료", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;
    let mut tray = TrayIconBuilder::new()
        .tooltip("CollaBot Overlay Bridge")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_window(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    // An installer registers the link's scheme. This does it for a copy that was never installed,
    // which is every development build.
    #[cfg(any(windows, target_os = "linux"))]
    if let Err(error) = app.deep_link().register_all() {
        eprintln!("Could not register the link that starts the bridge: {error}");
    }

    let (shared, commands) = Shared::new();
    app.manage(shared.clone());

    // The dashboard opens the link when the user presses connect, also while the bridge runs.
    // Then it means: whatever you were waiting for, try now.
    let woken = shared.clone();
    app.deep_link()
        .on_open_url(move |_| woken.send(Command::Wake));

    let data_dir = match config::data_dir() {
        Some(directory) => directory,
        None => app.path().app_local_data_dir()?,
    };
    tauri::async_runtime::spawn(server::run(shared.clone(), config::allowed_origins()));
    tauri::async_runtime::spawn(discord::run(
        shared.clone(),
        config::discord_app(),
        TokenStore::new(data_dir),
        commands,
    ));

    // Keep the window up to date. It asks for the status once when it loads.
    let handle = app.handle().clone();
    let watched = shared.clone();
    tauri::async_runtime::spawn(async move {
        let mut snapshot = watched.snapshot();
        let mut local = watched.local();
        let mut warned = false;
        loop {
            // A bridge that cannot serve the pages is of no use, and one that the dashboard's
            // link started would fail unseen in the tray. Its window says what is wrong.
            if !warned && local.borrow_and_update().server_error.is_some() {
                warned = true;
                show_window(&handle);
            }
            let changed = tokio::select! {
                changed = snapshot.changed() => changed,
                changed = local.changed() => changed,
            };
            if changed.is_err() {
                return;
            }
            let _ = handle.emit(STATUS_EVENT, read_status(&watched));
        }
    });

    build_tray(app)?;

    // Started by the dashboard's link, the bridge stays out of the way: the dashboard shows how
    // things are going. Started by hand, it shows its window.
    let started_by_link = app.deep_link().get_current().ok().flatten().is_some();
    if !started_by_link {
        show_window(app.handle());
    }
    Ok(())
}

fn main() {
    tauri::Builder::default()
        // Has to be the first plugin: a second launch hands its arguments to the first and ends
        // before anything else is set up. Links among them go on to the deep-link plugin.
        .plugin(tauri_plugin_single_instance::init(|app, arguments, _| {
            if !is_link_launch(&arguments) {
                show_window(app);
            }
        }))
        .plugin(tauri_plugin_deep_link::init())
        .invoke_handler(tauri::generate_handler![
            get_status,
            request_authorization,
            forget_authorization
        ])
        .setup(setup)
        .on_window_event(|window, event| {
            // Closing the window leaves the bridge running in the tray.
            if let WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .run(tauri::generate_context!())
        .expect("the bridge could not start");
}
