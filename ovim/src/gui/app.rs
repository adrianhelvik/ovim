//! Tauri application shell shared by `ovim gui` and the `ovim-gui` desktop entry.

use super::{GuiBridge, GuiKeyInput, GuiSnapshot};
use crate::cli::FileArg;
use anyhow::{Context, Result};
use tauri::ipc::Channel;
use tauri::{DragDropEvent, Manager, RunEvent, State, WebviewWindow, WindowEvent};

#[tauri::command]
async fn gui_snapshot(
    bridge: State<'_, GuiBridge>,
    columns: u16,
    rows: u16,
) -> Result<GuiSnapshot, String> {
    bridge.snapshot(columns, rows).await
}

/// Attach a coalesced, event-driven snapshot stream to one webview.
#[tauri::command]
async fn gui_subscribe(
    bridge: State<'_, GuiBridge>,
    columns: u16,
    rows: u16,
    on_event: Channel<GuiSnapshot>,
) -> Result<(), String> {
    bridge.snapshot(columns, rows).await?;
    let mut updates = bridge.subscribe();
    if let Some(snapshot) = updates.borrow_and_update().clone() {
        on_event.send(snapshot).map_err(|error| error.to_string())?;
    }

    tauri::async_runtime::spawn(async move {
        while updates.changed().await.is_ok() {
            let update = updates.borrow_and_update().clone();
            let Some(snapshot) = update else { continue };
            if on_event.send(snapshot).is_err() {
                break;
            }
        }
    });
    Ok(())
}

#[tauri::command]
async fn gui_key(bridge: State<'_, GuiBridge>, input: GuiKeyInput) -> Result<(), String> {
    bridge.key(input).await
}

#[tauri::command]
async fn gui_paste(bridge: State<'_, GuiBridge>, text: String) -> Result<(), String> {
    bridge.paste(text).await
}

#[tauri::command]
async fn gui_set_cursor(
    bridge: State<'_, GuiBridge>,
    pane: usize,
    line: usize,
    display_column: usize,
) -> Result<(), String> {
    bridge.set_cursor(pane, line, display_column).await
}

#[tauri::command]
async fn gui_select_tab(bridge: State<'_, GuiBridge>, index: usize) -> Result<(), String> {
    bridge.select_tab(index).await
}

#[tauri::command]
async fn gui_focus_pane(bridge: State<'_, GuiBridge>, index: usize) -> Result<(), String> {
    bridge.focus_pane(index).await
}

#[tauri::command]
async fn gui_select_picker(bridge: State<'_, GuiBridge>, index: usize) -> Result<(), String> {
    bridge.select_picker(index).await
}

#[tauri::command]
async fn gui_select_file_tree(
    bridge: State<'_, GuiBridge>,
    index: usize,
    activate: bool,
) -> Result<(), String> {
    bridge.select_file_tree(index, activate).await
}

#[tauri::command]
async fn gui_select_problem(
    bridge: State<'_, GuiBridge>,
    kind: String,
    index: usize,
    activate: bool,
) -> Result<(), String> {
    bridge.select_problem(kind, index, activate).await
}

#[tauri::command]
async fn gui_select_lsp(
    bridge: State<'_, GuiBridge>,
    index: usize,
    activate: bool,
) -> Result<(), String> {
    bridge.select_lsp(index, activate).await
}

#[tauri::command]
fn gui_window_action(window: WebviewWindow, action: String) -> Result<(), String> {
    match action.as_str() {
        "minimize" => window.minimize(),
        "toggle-maximize" => window.is_maximized().and_then(|maximized| {
            if maximized {
                window.unmaximize()
            } else {
                window.maximize()
            }
        }),
        "close" => window.close(),
        _ => return Err(format!("Unknown window action: {action}")),
    }
    .map_err(|error| error.to_string())
}

/// Run the native application on the calling thread until its last window closes.
pub fn run(file: Option<FileArg>, resume: bool) -> Result<()> {
    // Keep Tauri's patchable bundle marker linked even without the updater
    // plugin. The bundler uses it to distinguish deb/AppImage/MSI installs.
    std::hint::black_box(tauri::utils::platform::bundle_type());
    crate::lsp_init::set_headless_mode(false);
    let _ = crate::lsp::init_lsp_logging();

    let bridge = GuiBridge::spawn(file, resume)?;
    let shutdown_bridge = bridge.clone();
    let application = tauri::Builder::default()
        .manage(bridge)
        .invoke_handler(tauri::generate_handler![
            gui_snapshot,
            gui_subscribe,
            gui_key,
            gui_paste,
            gui_set_cursor,
            gui_select_tab,
            gui_focus_pane,
            gui_select_picker,
            gui_select_file_tree,
            gui_select_problem,
            gui_select_lsp,
            gui_window_action,
        ])
        .setup(|app| {
            if let Some(window) = app.get_webview_window("main") {
                window
                    .set_title("Ovim")
                    .context("Failed to set the GUI window title")?;
                let drop_bridge = app.state::<GuiBridge>().inner().clone();
                window.on_window_event(move |event| {
                    let WindowEvent::DragDrop(DragDropEvent::Drop { paths, .. }) = event else {
                        return;
                    };
                    let bridge = drop_bridge.clone();
                    let paths = paths.clone();
                    tauri::async_runtime::spawn(async move {
                        let _ = bridge.attach_images(paths).await;
                    });
                });
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .context("Failed to build the Ovim GUI")?;

    application.run(move |_handle, event| {
        if matches!(event, RunEvent::Exit | RunEvent::ExitRequested { .. }) {
            shutdown_bridge.shutdown();
        }
    });
    Ok(())
}
