import type { GuiSnapshot } from "./types";

export type WorkbenchLayoutPreference = {
    activeDock: "explorer" | "context";
    activeContextPanel: "ai" | "tests" | "debug";
};

export const workspaceLayoutIdentity = (
    snapshot: Pick<GuiSnapshot, "filePath" | "workspacePath" | "projectName">,
) => {
    const workspace = snapshot.workspacePath?.replaceAll("\\", "/");
    if (workspace) return workspace;
    const path = snapshot.filePath?.replaceAll("\\", "/");
    if (path) {
        const parts = path.split("/").filter(Boolean);
        const projectIndex = parts.lastIndexOf(snapshot.projectName);
        if (projectIndex >= 0) {
            const prefix = path.startsWith("/") ? "/" : "";
            return prefix + parts.slice(0, projectIndex + 1).join("/");
        }
    }
    return snapshot.projectName || "ovim";
};

const storageKey = (workspace: string) =>
    `ovim.gui.layout.v1.${encodeURIComponent(workspace)}`;

export const readWorkbenchLayout = (
    storage: Pick<Storage, "getItem"> | undefined,
    workspace: string,
): WorkbenchLayoutPreference | undefined => {
    if (!storage) return undefined;
    try {
        const parsed = JSON.parse(
            storage.getItem(storageKey(workspace)) ?? "",
        ) as Partial<WorkbenchLayoutPreference> | undefined;
        if (
            !parsed ||
            !["explorer", "context"].includes(parsed.activeDock ?? "") ||
            !["ai", "tests", "debug"].includes(parsed.activeContextPanel ?? "")
        )
            return undefined;
        return parsed as WorkbenchLayoutPreference;
    } catch {
        return undefined;
    }
};

export const writeWorkbenchLayout = (
    storage: Pick<Storage, "setItem"> | undefined,
    workspace: string,
    preference: WorkbenchLayoutPreference,
) => {
    if (!storage) return;
    try {
        storage.setItem(storageKey(workspace), JSON.stringify(preference));
    } catch {
        // Layout persistence is optional when storage is unavailable or full.
    }
};
