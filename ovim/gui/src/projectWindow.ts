/**
 * Native "File → Open Project…" / "File → New Window" both open a project in a
 * separate operating-system window, which the Rust side implements as a
 * detached sibling process. The webview only decides which entry point to call
 * and with which directory, so that decision lives here as a pure mapping.
 */
export interface ProjectWindowInvocation {
    command: "gui_open_project_dialog" | "gui_open_project_window";
    args: Record<string, string>;
}

export type ProjectWindowAction = "file.open-project" | "file.new-window";

export const projectWindowInvocation = (
    action: string,
    workspacePath: string | undefined,
): ProjectWindowInvocation | undefined => {
    if (action === "file.open-project")
        return { command: "gui_open_project_dialog", args: {} };
    if (action !== "file.new-window") return undefined;

    // "New Window" reopens the current project. Without one — a lone file, or
    // the dashboard — there is nothing to duplicate, so ask instead of
    // silently opening an unrelated directory.
    const workspace = workspacePath?.trim();
    return workspace
        ? { command: "gui_open_project_window", args: { path: workspace } }
        : { command: "gui_open_project_dialog", args: {} };
};
