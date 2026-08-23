/** @vitest-environment jsdom */

import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { createSignal, onMount } from "solid-js";
import { afterEach, describe, expect, it } from "vitest";
import ContextDock, { type ContextPanelDefinition } from "./ContextDock";

afterEach(cleanup);

const panel = (
    id: ContextPanelDefinition["id"],
    label: string,
): ContextPanelDefinition => ({
    id,
    label,
    state: "ready",
    icon: id === "ai" ? "ai-spark" : id === "tests" ? "test" : "debug",
    component: () => <p>{label} content</p>,
});

describe("ContextDock", () => {
    it("mounts one context surface and switches it through accessible tabs", () => {
        render(() => (
            <ContextDock
                panels={[
                    panel("ai", "AI chat"),
                    panel("tests", "Tests"),
                    panel("debug", "Debug"),
                ]}
            />
        ));

        expect(screen.getByRole("tabpanel").textContent).toContain(
            "AI chat content",
        );
        expect(screen.queryByText("Tests content")).toBeNull();

        fireEvent.click(screen.getByRole("tab", { name: "Tests" }));
        expect(screen.getByRole("tabpanel").textContent).toContain(
            "Tests content",
        );
        expect(screen.queryByText("AI chat content")).toBeNull();
    });

    it("moves tab focus with arrow keys and recovers when a panel closes", async () => {
        const [panels, setPanels] = createSignal([
            panel("ai", "AI chat"),
            panel("tests", "Tests"),
        ]);
        render(() => <ContextDock panels={panels()} />);

        const ai = screen.getByRole("tab", { name: "AI chat" });
        fireEvent.keyDown(ai, { key: "ArrowRight" });
        await Promise.resolve();
        expect(document.activeElement).toBe(
            screen.getByRole("tab", { name: "Tests" }),
        );
        expect(screen.getByRole("tabpanel").textContent).toContain(
            "Tests content",
        );

        setPanels([panel("ai", "AI chat")]);
        expect(
            screen.getByRole("tabpanel", { name: "AI chat" }).textContent,
        ).toContain("AI chat content");
    });

    it("honors a controlled active panel without undoing user selection", () => {
        const [active, setActive] =
            createSignal<ContextPanelDefinition["id"]>("ai");
        render(() => (
            <ContextDock
                panels={[panel("ai", "AI chat"), panel("tests", "Tests")]}
                activePanel={active()}
                onActivePanel={setActive}
            />
        ));

        fireEvent.click(screen.getByRole("tab", { name: "Tests" }));
        expect(active()).toBe("tests");
        expect(screen.getByRole("tabpanel").textContent).toContain(
            "Tests content",
        );
    });

    it("preserves the active surface when snapshot metadata changes", () => {
        let mounts = 0;
        const StablePanel = () => {
            onMount(() => mounts++);
            return <input aria-label="Persistent input" />;
        };
        const [panels, setPanels] = createSignal<ContextPanelDefinition[]>([
            {
                ...panel("ai", "AI chat"),
                state: "idle",
                component: StablePanel,
            },
            panel("tests", "Tests"),
        ]);
        render(() => <ContextDock panels={panels()} />);
        const input = screen.getByRole("textbox", {
            name: "Persistent input",
        });
        const tab = screen.getByRole("tab", { name: "AI chat" });

        setPanels([
            {
                ...panel("ai", "AI chat"),
                state: "streaming",
                component: StablePanel,
            },
            panel("tests", "Tests"),
        ]);

        expect(screen.getByRole("textbox", { name: "Persistent input" })).toBe(
            input,
        );
        expect(screen.getByRole("tab", { name: "AI chat" })).toBe(tab);
        expect(mounts).toBe(1);
    });
});
