/** @vitest-environment jsdom */

import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import GdiffPanel from "./GdiffPanel";
import type { GuiGdiffReview } from "./types";

const invoke = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({ invoke }));

const runningReview = (): GuiGdiffReview => ({
    installed: true,
    running: true,
    repo: "ovim",
    spec: "main...WORKTREE",
    displaySpec: "main...WORKTREE",
    files: ["ovim/gui/src/App.tsx", "ovim-core/src/gdiff.rs"],
    comments: [
        {
            path: "ovim/gui/src/App.tsx",
            line: 42,
            text: "Keep the interaction keyboard-accessible.",
        },
    ],
});

beforeEach(() => {
    invoke.mockReset();
});

afterEach(() => {
    vi.restoreAllMocks();
    document.body.replaceChildren();
});

describe("GdiffPanel", () => {
    it("refreshes, adds, selects, and removes shared review comments", async () => {
        const review = runningReview();
        invoke.mockImplementation(
            (command: string, args?: Record<string, unknown>) => {
                if (command === "gui_gdiff_state")
                    return Promise.resolve(review);
                if (command === "gui_gdiff_comment") {
                    if (args?.action === "add") {
                        return Promise.resolve([
                            ...review.comments,
                            {
                                path: args.path,
                                line: args.line,
                                text: args.text,
                            },
                        ]);
                    }
                    return Promise.resolve([]);
                }
                return Promise.reject(
                    new Error(`Unexpected command: ${command}`),
                );
            },
        );
        const onReview = vi.fn();
        const result = render(() => (
            <GdiffPanel
                native
                workspace="/workspace/ovim"
                onReview={onReview}
            />
        ));
        try {
            expect(
                (
                    await screen.findByRole("region", {
                        name: "Active comparison",
                    })
                ).textContent,
            ).toContain("main...WORKTREE");

            const stateCalls = invoke.mock.calls.filter(
                ([command]) => command === "gui_gdiff_state",
            ).length;
            fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
            await waitFor(() => {
                expect(
                    invoke.mock.calls.filter(
                        ([command]) => command === "gui_gdiff_state",
                    ).length,
                ).toBeGreaterThan(stateCalls);
                expect(
                    (
                        screen.getByRole("button", {
                            name: "Refresh",
                        }) as HTMLButtonElement
                    ).disabled,
                ).toBe(false);
            });

            fireEvent.change(screen.getByLabelText("Changed file"), {
                target: { value: "ovim-core/src/gdiff.rs" },
            });
            fireEvent.input(screen.getByLabelText("New-side line"), {
                target: { value: "17" },
            });
            fireEvent.input(screen.getByLabelText("Review note"), {
                target: { value: "Bound this response." },
            });
            fireEvent.click(
                screen.getByRole("button", { name: "Share with Gdiff" }),
            );
            await waitFor(() =>
                expect(invoke).toHaveBeenCalledWith("gui_gdiff_comment", {
                    action: "add",
                    path: "ovim-core/src/gdiff.rs",
                    line: 17,
                    text: "Bound this response.",
                }),
            );
            expect(
                (screen.getByLabelText("Review note") as HTMLTextAreaElement)
                    .value,
            ).toBe("");

            fireEvent.click(
                screen.getByRole("button", {
                    name: "Remove comment at ovim/gui/src/App.tsx:42",
                }),
            );
            await waitFor(() =>
                expect(invoke).toHaveBeenCalledWith("gui_gdiff_comment", {
                    action: "remove",
                    path: "ovim/gui/src/App.tsx",
                    line: 42,
                    text: "",
                }),
            );
            expect(onReview).toHaveBeenCalled();
        } finally {
            result.unmount();
        }
    });

    it("starts a missing review and surfaces command failures", async () => {
        const stopped = { ...runningReview(), running: false, comments: [] };
        const running = runningReview();
        let states = 0;
        invoke.mockImplementation((command: string) => {
            if (command === "gui_gdiff_state") {
                states += 1;
                return Promise.resolve(states === 1 ? stopped : running);
            }
            if (command === "gui_gdiff_start") return Promise.resolve();
            return Promise.reject(new Error("Gdiff command failed"));
        });

        const result = render(() => (
            <GdiffPanel
                native
                workspace="/workspace/ovim"
                onReview={() => {}}
            />
        ));
        try {
            fireEvent.click(
                await screen.findByRole("button", { name: "Open Gdiff" }),
            );
            expect(
                (
                    await screen.findByRole("region", {
                        name: "Active comparison",
                    })
                ).textContent,
            ).toContain("main...WORKTREE");
            expect(invoke).toHaveBeenCalledWith("gui_gdiff_start");
        } finally {
            result.unmount();
        }
    });
});
