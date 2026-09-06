import { describe, expect, it } from "vitest";
import { projectWindowInvocation } from "./projectWindow";

describe("project window menu actions", () => {
    it("opens the native picker for Open Project", () => {
        expect(
            projectWindowInvocation("file.open-project", "/workspace/ovim"),
        ).toEqual({ command: "gui_open_project_dialog", args: {} });
    });

    it("reopens the current workspace for New Window", () => {
        expect(
            projectWindowInvocation("file.new-window", "/workspace/ovim"),
        ).toEqual({
            command: "gui_open_project_window",
            args: { path: "/workspace/ovim" },
        });
    });

    it("falls back to the picker when no project is open", () => {
        for (const workspace of [undefined, "", "   "])
            expect(
                projectWindowInvocation("file.new-window", workspace),
            ).toEqual({ command: "gui_open_project_dialog", args: {} });
    });

    it("ignores unrelated menu actions", () => {
        expect(
            projectWindowInvocation("file.save", "/workspace/ovim"),
        ).toBeUndefined();
    });
});
