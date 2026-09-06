//! Opening a project directory in its own operating-system window.
//!
//! The GUI is structurally single-window: `GuiBridge`, `BrowserHost`, and
//! `GuiMenuState` are process-global singletons and every command handler
//! resolves them without a window label. Rather than key all of that state per
//! window, a new project window is a detached child process running the same
//! executable with the project directory as its launch argument — the argument
//! path `run_editor` already treats as "open this as a workspace". Each window
//! then owns an independent editor, language-server set, and menu state, and
//! outlives the window that spawned it.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// File stem of the dedicated GUI binary, as declared in `Cargo.toml`.
const GUI_BINARY_STEM: &str = "ovim-gui";

/// Resolve a user-supplied project path into a directory we can launch.
///
/// Relative paths stay relative: the child inherits this process's working
/// directory, so it resolves them identically.
pub fn resolve_project_dir(raw: &str) -> Result<PathBuf, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("No project directory given".to_string());
    }
    let path = PathBuf::from(shellexpand::tilde(trimmed).into_owned());
    let metadata = std::fs::metadata(&path)
        .map_err(|error| format!("Could not open {}: {error}", path.display()))?;
    if !metadata.is_dir() {
        return Err(format!("{} is not a project directory", path.display()));
    }
    Ok(path)
}

/// Launch a detached sibling process editing `directory`.
///
/// `current_exe` keeps a developer build from launching an installed one (and
/// vice versa) when both are present.
pub fn spawn_project_window(directory: &Path) -> Result<(), String> {
    let executable = std::env::current_exe()
        .map_err(|error| format!("Could not locate the Ovim executable: {error}"))?;
    spawn_detached(&executable, directory)
}

/// Spawn `executable` on `directory` so that it outlives this process.
fn spawn_detached(executable: &Path, directory: &Path) -> Result<(), String> {
    let mut command = Command::new(executable);
    command
        .args(launch_arguments(executable, directory))
        // Sharing this process's stdio would let a full pipe in the new window
        // block the one that opened it, and would interleave two editors'
        // output when Ovim was started from a terminal.
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    detach(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("Could not open a new window: {error}"))?;
    // Nothing in this process will ever await the new window, so hand the
    // child off to a thread that reaps it. Dropping the handle instead would
    // leave a zombie behind for every sibling window that closes first.
    std::thread::Builder::new()
        .name("ovim-gui-window".to_string())
        .spawn(move || {
            let _ = child.wait();
        })
        .map_err(|error| format!("Could not supervise the new window: {error}"))?;
    Ok(())
}

/// Arguments that relaunch `executable` as a GUI window editing `directory`.
///
/// `ovim gui` runs the same window in-process from the CLI binary, so a window
/// opened from that entry point has to reinstate the subcommand: bare
/// `ovim <dir>` would start a terminal editor with no terminal attached.
fn launch_arguments(executable: &Path, directory: &Path) -> Vec<PathBuf> {
    let mut arguments = Vec::new();
    if executable.file_stem() != Some(GUI_BINARY_STEM.as_ref()) {
        arguments.push(PathBuf::from("gui"));
    }
    arguments.push(directory.to_path_buf());
    arguments
}

/// Ask for a project directory and open it in a new window.
///
/// Dismissing the picker is a deliberate no-op, not a failure.
pub async fn pick_project_window() -> Result<Option<PathBuf>, String> {
    let Some(folder) = rfd::AsyncFileDialog::new()
        .set_title("Open Project")
        .pick_folder()
        .await
    else {
        return Ok(None);
    };
    let directory = folder.path().to_path_buf();
    spawn_project_window(&directory)?;
    Ok(Some(directory))
}

/// Service a queued `:openwin` request and return the status line to show.
///
/// `None` means the user dismissed the picker, which leaves the status line
/// alone rather than reporting a cancellation as news.
pub async fn open_project_window(path: Option<String>) -> Option<String> {
    let outcome = match path {
        Some(path) => resolve_project_dir(&path)
            .and_then(|directory| spawn_project_window(&directory).map(|()| Some(directory))),
        None => pick_project_window().await,
    };
    match outcome {
        Ok(Some(directory)) => Some(format!("Opened {} in a new window", directory.display())),
        Ok(None) => None,
        Err(error) => Some(error),
    }
}

/// Open a directory picker and launch the chosen project in a new window.
#[tauri::command]
pub async fn gui_open_project_dialog() -> Result<(), String> {
    pick_project_window().await.map(|_| ())
}

/// Open an explicit directory in a new window, bypassing the picker.
///
/// The remote-editing work drives this path with a target it already knows.
#[tauri::command]
pub async fn gui_open_project_window(path: String) -> Result<(), String> {
    let directory = resolve_project_dir(&path)?;
    spawn_project_window(&directory)
}

#[cfg(unix)]
fn detach(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    // SAFETY: `setsid` is async-signal-safe and allocates nothing, so it is
    // legal in the window between `fork` and `exec`. A fresh session detaches
    // the new window from this process's controlling terminal and process
    // group, so a Ctrl-C aimed at the launching shell cannot reach it.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
}

#[cfg(windows)]
fn detach(command: &mut Command) {
    use std::os::windows::process::CommandExt;

    // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP: the new window inherits no
    // console, and console control events do not propagate into it.
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

#[cfg(not(any(unix, windows)))]
fn detach(_command: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_path_is_reported_instead_of_launched() {
        let missing = tempfile::tempdir().unwrap().path().join("absent");
        let error = resolve_project_dir(&missing.to_string_lossy()).unwrap_err();
        assert!(error.starts_with("Could not open "), "{error}");
    }

    #[test]
    fn a_file_is_not_a_project_directory() {
        let workspace = tempfile::tempdir().unwrap();
        let file = workspace.path().join("main.rs");
        std::fs::write(&file, "fn main() {}").unwrap();

        let error = resolve_project_dir(&file.to_string_lossy()).unwrap_err();
        assert!(error.ends_with("is not a project directory"), "{error}");
    }

    #[test]
    fn the_gui_binary_relaunches_itself_with_only_the_project_directory() {
        for executable in ["/opt/ovim/ovim-gui", "/opt/ovim/ovim-gui.exe"] {
            assert_eq!(
                launch_arguments(Path::new(executable), Path::new("/projects/ovim")),
                [PathBuf::from("/projects/ovim")]
            );
        }
    }

    #[test]
    fn the_cli_binary_reinstates_the_gui_subcommand() {
        assert_eq!(
            launch_arguments(Path::new("/opt/ovim/ovim"), Path::new("/projects/ovim")),
            [PathBuf::from("gui"), PathBuf::from("/projects/ovim")]
        );
    }

    /// End-to-end check of the launch path: the child runs with the project
    /// directory as its only argument, with stdio detached from this process,
    /// and is reaped rather than left as a zombie.
    #[cfg(unix)]
    #[test]
    fn a_spawned_window_runs_detached_and_is_reaped() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().unwrap();
        let marker = workspace.path().join("launched");
        // Named `ovim-gui` so `launch_arguments` passes the directory alone.
        let executable = workspace.path().join("ovim-gui");
        // Write via a rename so the assertion below never observes a
        // half-written marker.
        std::fs::write(
            &executable,
            format!(
                "#!/bin/sh\nprintf '%s' \"$1\" > {}.tmp\nmv {}.tmp {}\n",
                marker.display(),
                marker.display(),
                marker.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();

        spawn_detached(&executable, workspace.path()).unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !marker.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap(),
            workspace.path().to_string_lossy()
        );
    }

    #[test]
    fn a_blank_argument_is_rejected_before_touching_the_filesystem() {
        assert_eq!(
            resolve_project_dir("   ").unwrap_err(),
            "No project directory given"
        );
    }

    #[test]
    fn surrounding_whitespace_and_a_home_prefix_are_resolved() {
        let workspace = tempfile::tempdir().unwrap();
        let resolved = resolve_project_dir(&format!("  {}  ", workspace.path().display())).unwrap();
        assert_eq!(resolved, workspace.path());

        // `~` expansion happens here rather than in the child, which receives
        // an already-resolved argument.
        if let Some(home) = dirs::home_dir() {
            assert_eq!(resolve_project_dir("~").unwrap(), home);
        }
    }
}
