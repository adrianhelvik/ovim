/** @vitest-environment jsdom */

import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vitest";
import SurfaceCommandLine from "./SurfaceCommandLine";

afterEach(() => document.body.replaceChildren());

describe("SurfaceCommandLine", () => {
    it("focuses, completes, and executes in its named surface context", async () => {
        const execute = vi.fn().mockResolvedValue({ ok: true });
        const dismiss = vi.fn();
        const [serial, setSerial] = createSignal(1);
        render(() => (
            <SurfaceCommandLine
                active
                requestSerial={serial()}
                surface="browser"
                completions={["back", "goto", "reload"]}
                onExecute={execute}
                onDismiss={dismiss}
            />
        ));

        const input = screen.getByLabelText(
            "browser command input",
        ) as HTMLInputElement;
        await waitFor(() => expect(document.activeElement).toBe(input));
        fireEvent.input(input, { target: { value: "rel" } });
        fireEvent.keyDown(input, { key: "Tab" });
        expect(input.value).toBe("reload");
        fireEvent.submit(input.closest("form")!);
        await waitFor(() => expect(execute).toHaveBeenCalledWith("reload"));
        expect(dismiss).toHaveBeenCalledOnce();

        setSerial(2);
        await waitFor(() => expect(input.value).toBe(""));
    });

    it("keeps contextual errors visible and dismisses with escape", async () => {
        const dismiss = vi.fn();
        render(() => (
            <SurfaceCommandLine
                active
                requestSerial={1}
                surface="browser"
                completions={[]}
                onExecute={async () => ({
                    ok: false,
                    message: ":write is unavailable in a browser tab",
                })}
                onDismiss={dismiss}
            />
        ));
        const input = screen.getByLabelText("browser command input");
        fireEvent.input(input, { target: { value: "w" } });
        fireEvent.submit(input.closest("form")!);
        expect(
            await screen.findByText(":write is unavailable in a browser tab"),
        ).toBeTruthy();
        fireEvent.keyDown(input, { key: "Escape" });
        expect(dismiss).toHaveBeenCalledOnce();
    });
});
