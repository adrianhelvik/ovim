//! Native menu construction and context-aware accelerator ownership.
//!
//! A Tauri child webview cannot observe keyboard events in the main webview,
//! so native menu accelerators are the shared shortcut boundary. Browser-only
//! accelerators must be released whenever the source editor owns the workbench;
//! otherwise Ctrl-W, Ctrl-T, and friends never reach Vim on non-macOS hosts.

use anyhow::Result;
use serde::Deserialize;
use tauri::menu::{MenuBuilder, MenuItem, SubmenuBuilder};
use tauri::{App, State, Wry};

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum GuiMenuSurface {
    Source,
    Browser,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuiMenuPlatform {
    Macos,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GuiMenuPolicy {
    new_tab_accelerator: Option<&'static str>,
    restore_tab_accelerator: Option<&'static str>,
    close_accelerator: Option<&'static str>,
    browser_navigation_enabled: bool,
}

impl GuiMenuPolicy {
    fn for_surface(
        surface: GuiMenuSurface,
        platform: GuiMenuPlatform,
        can_restore_browser: bool,
    ) -> Self {
        let browser_active = surface == GuiMenuSurface::Browser;
        let conventional_macos_shortcuts = platform == GuiMenuPlatform::Macos;
        Self {
            new_tab_accelerator: (browser_active || conventional_macos_shortcuts)
                .then_some("CmdOrCtrl+T"),
            restore_tab_accelerator: (can_restore_browser
                && (browser_active || conventional_macos_shortcuts))
                .then_some("CmdOrCtrl+Shift+T"),
            close_accelerator: (browser_active || conventional_macos_shortcuts)
                .then_some("CmdOrCtrl+W"),
            browser_navigation_enabled: browser_active,
        }
    }
}

#[derive(Clone)]
pub struct GuiMenuState {
    new_browser_tab: MenuItem<Wry>,
    restore_browser_tab: MenuItem<Wry>,
    close: MenuItem<Wry>,
    browser_navigation: Vec<(MenuItem<Wry>, &'static str)>,
}

impl GuiMenuState {
    fn apply(&self, surface: GuiMenuSurface, can_restore_browser: bool) -> tauri::Result<()> {
        let platform = if cfg!(target_os = "macos") {
            GuiMenuPlatform::Macos
        } else {
            GuiMenuPlatform::Other
        };
        let policy = GuiMenuPolicy::for_surface(surface, platform, can_restore_browser);
        self.new_browser_tab
            .set_accelerator(policy.new_tab_accelerator)?;
        self.restore_browser_tab.set_enabled(can_restore_browser)?;
        self.restore_browser_tab
            .set_accelerator(policy.restore_tab_accelerator)?;
        self.close.set_accelerator(policy.close_accelerator)?;
        for (item, accelerator) in &self.browser_navigation {
            item.set_enabled(policy.browser_navigation_enabled)?;
            item.set_accelerator(policy.browser_navigation_enabled.then_some(*accelerator))?;
        }
        Ok(())
    }
}

#[tauri::command]
pub fn gui_set_menu_surface(
    menu: State<'_, GuiMenuState>,
    surface: GuiMenuSurface,
    can_restore_browser: bool,
) -> Result<(), String> {
    menu.apply(surface, can_restore_browser)
        .map_err(|error| error.to_string())
}

pub fn install(app: &App) -> Result<GuiMenuState> {
    let new_browser_tab = MenuItem::with_id(
        app,
        "browser.new-tab",
        "New Browser Tab",
        true,
        None::<&str>,
    )?;
    let restore_browser_tab = MenuItem::with_id(
        app,
        "browser.restore-tab",
        "Reopen Closed Browser Tab",
        false,
        None::<&str>,
    )?;
    let save = MenuItem::with_id(app, "file.save", "Save", true, Some("CmdOrCtrl+S"))?;
    let save_all = MenuItem::with_id(
        app,
        "file.save-all",
        "Save All",
        true,
        Some("CmdOrCtrl+Alt+S"),
    )?;
    let close = MenuItem::with_id(app, "file.close", "Close", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "app.quit", "Quit Ovim", true, Some("CmdOrCtrl+Q"))?;
    let undo = MenuItem::with_id(app, "edit.undo", "Undo", true, Some("CmdOrCtrl+Z"))?;
    let redo = MenuItem::with_id(app, "edit.redo", "Redo", true, Some("CmdOrCtrl+Shift+Z"))?;
    let select_all = MenuItem::with_id(
        app,
        "edit.select-all",
        "Select All",
        true,
        Some("CmdOrCtrl+A"),
    )?;
    let find = MenuItem::with_id(app, "edit.find", "Find", true, Some("CmdOrCtrl+F"))?;
    let focus_address = MenuItem::with_id(
        app,
        "browser.focus-address",
        "Open Location",
        false,
        None::<&str>,
    )?;
    let back = MenuItem::with_id(app, "browser.back", "Back", false, None::<&str>)?;
    let forward = MenuItem::with_id(app, "browser.forward", "Forward", false, None::<&str>)?;
    let reload = MenuItem::with_id(app, "browser.reload", "Reload", false, None::<&str>)?;
    let previous_tab = MenuItem::with_id(
        app,
        "browser.previous-tab",
        "Previous Tab",
        false,
        None::<&str>,
    )?;
    let next_tab = MenuItem::with_id(app, "browser.next-tab", "Next Tab", false, None::<&str>)?;

    let app_menu = SubmenuBuilder::new(app, "Ovim")
        .about(None)
        .separator()
        .hide()
        .hide_others()
        .show_all()
        .separator()
        .item(&quit)
        .build()?;
    let file_menu = SubmenuBuilder::new(app, "File")
        .item(&new_browser_tab)
        .item(&restore_browser_tab)
        .separator()
        .item(&save)
        .item(&save_all)
        .separator()
        .item(&close)
        .build()?;
    let edit_menu = SubmenuBuilder::new(app, "Edit")
        .item(&undo)
        .item(&redo)
        .separator()
        .cut()
        .copy()
        .paste()
        .item(&select_all)
        .separator()
        .item(&find)
        .build()?;
    let view_menu = SubmenuBuilder::new(app, "View").fullscreen().build()?;
    let navigation_menu = SubmenuBuilder::new(app, "Navigate")
        .item(&focus_address)
        .separator()
        .item(&back)
        .item(&forward)
        .item(&reload)
        .separator()
        .item(&previous_tab)
        .item(&next_tab)
        .build()?;
    let window_menu = SubmenuBuilder::new(app, "Window")
        .minimize()
        .maximize()
        .separator()
        .bring_all_to_front()
        .build()?;
    let menu = MenuBuilder::new(app)
        .items(&[
            &app_menu,
            &file_menu,
            &edit_menu,
            &view_menu,
            &navigation_menu,
            &window_menu,
        ])
        .build()?;
    app.set_menu(menu)?;

    let state = GuiMenuState {
        new_browser_tab,
        restore_browser_tab,
        close,
        browser_navigation: vec![
            (focus_address, "CmdOrCtrl+L"),
            (back, "CmdOrCtrl+["),
            (forward, "CmdOrCtrl+]"),
            (reload, "CmdOrCtrl+R"),
            (previous_tab, "CmdOrCtrl+Shift+["),
            (next_tab, "CmdOrCtrl+Shift+]"),
        ],
    };
    state.apply(GuiMenuSurface::Source, false)?;
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_macos_source_surface_releases_vim_control_keys() {
        let policy =
            GuiMenuPolicy::for_surface(GuiMenuSurface::Source, GuiMenuPlatform::Other, true);
        assert_eq!(policy.new_tab_accelerator, None);
        assert_eq!(policy.restore_tab_accelerator, None);
        assert_eq!(policy.close_accelerator, None);
        assert!(!policy.browser_navigation_enabled);
    }

    #[test]
    fn browser_surface_owns_its_native_shortcuts() {
        for platform in [GuiMenuPlatform::Macos, GuiMenuPlatform::Other] {
            let policy = GuiMenuPolicy::for_surface(GuiMenuSurface::Browser, platform, true);
            assert_eq!(policy.new_tab_accelerator, Some("CmdOrCtrl+T"));
            assert_eq!(policy.restore_tab_accelerator, Some("CmdOrCtrl+Shift+T"));
            assert_eq!(policy.close_accelerator, Some("CmdOrCtrl+W"));
            assert!(policy.browser_navigation_enabled);
        }
    }

    #[test]
    fn macos_keeps_conventional_tab_and_window_shortcuts() {
        let policy =
            GuiMenuPolicy::for_surface(GuiMenuSurface::Source, GuiMenuPlatform::Macos, true);
        assert_eq!(policy.new_tab_accelerator, Some("CmdOrCtrl+T"));
        assert_eq!(policy.restore_tab_accelerator, Some("CmdOrCtrl+Shift+T"));
        assert_eq!(policy.close_accelerator, Some("CmdOrCtrl+W"));
        assert!(!policy.browser_navigation_enabled);
    }
}
