// @vitest-environment jsdom
import { fireEvent, render, waitFor } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import DiffPanel from "./DiffPanel";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const review = {
    root: "/repo",
    spec: "HEAD...WORKTREE",
    displaySpec: "HEAD...working tree",
    files: [
        {
            path: "src/main.rs",
            status: "modified",
            additions: 3,
            deletions: 1,
            binary: false,
        },
    ],
};

describe("DiffPanel", () => {
    beforeEach(() => {
        vi.mocked(invoke).mockReset();
        vi.mocked(invoke).mockResolvedValue(review);
    });

    it("loads native changes and opens a selected file as a diff buffer", async () => {
        const result = render(() => <DiffPanel native />);
        const file = await result.findByRole("listitem", {
            name: /src\/main.rs/i,
        });
        expect(invoke).toHaveBeenCalledWith("gui_diff_state", {
            spec: "HEAD...WORKTREE",
        });

        await fireEvent.click(file);
        await waitFor(() =>
            expect(invoke).toHaveBeenCalledWith("gui_diff_open_file", {
                spec: "HEAD...WORKTREE",
                path: "src/main.rs",
            }),
        );
    });

    it("refreshes a user-entered comparison", async () => {
        const result = render(() => <DiffPanel native />);
        const input = await result.findByRole("textbox", {
            name: /Comparison/,
        });
        await fireEvent.input(input, { target: { value: "main...WORKTREE" } });
        await fireEvent.keyDown(input, { key: "Enter" });
        await waitFor(() =>
            expect(invoke).toHaveBeenCalledWith("gui_diff_state", {
                spec: "main...WORKTREE",
            }),
        );
    });

    it("surfaces native diff failures", async () => {
        vi.mocked(invoke).mockRejectedValueOnce(new Error("not a repository"));
        const result = render(() => <DiffPanel native />);
        expect((await result.findByRole("alert")).textContent).toContain(
            "not a repository",
        );
    });
});
