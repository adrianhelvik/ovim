/** @vitest-environment jsdom */

import { describe, expect, it } from "vitest";
import { isGuiNativeControl, trapDialogFocus } from "./focus";

describe("dialog focus containment", () => {
    it("wraps focus in both directions", () => {
        const dialog = document.createElement("section");
        const first = document.createElement("button");
        const last = document.createElement("button");
        dialog.append(first, last);
        document.body.append(dialog);

        last.focus();
        const forward = new KeyboardEvent("keydown", {
            key: "Tab",
            cancelable: true,
        });
        expect(trapDialogFocus(forward, dialog)).toBe(true);
        expect(document.activeElement).toBe(first);

        const backward = new KeyboardEvent("keydown", {
            key: "Tab",
            shiftKey: true,
            cancelable: true,
        });
        expect(trapDialogFocus(backward, dialog)).toBe(true);
        expect(document.activeElement).toBe(last);
        dialog.remove();
    });
});

describe("native control focus", () => {
    it("preserves toolbar inputs without treating the editor sink as native", () => {
        const toolbar = document.createElement("form");
        const address = document.createElement("input");
        const addressIcon = document.createElement("span");
        addressIcon.dataset.guiNativeControl = "";
        toolbar.append(address, addressIcon);
        document.body.append(toolbar);

        expect(isGuiNativeControl(address)).toBe(true);
        expect(isGuiNativeControl(addressIcon)).toBe(true);
        expect(isGuiNativeControl(address, address)).toBe(false);

        toolbar.remove();
    });
});
