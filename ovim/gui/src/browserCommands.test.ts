import { describe, expect, it } from "vitest";
import {
    normalizeBrowserAddress,
    parseBrowserCommand,
} from "./browserCommands";

describe("browser commands", () => {
    it("normalizes human addresses and preserves explicit schemes", () => {
        expect(normalizeBrowserAddress("wikipedia.org")).toBe(
            "https://wikipedia.org",
        );
        expect(normalizeBrowserAddress("http://localhost:3000/docs")).toBe(
            "http://localhost:3000/docs",
        );
        expect(normalizeBrowserAddress("localhost:3000/docs")).toBe(
            "http://localhost:3000/docs",
        );
        expect(normalizeBrowserAddress("example.com:8443/docs")).toBe(
            "https://example.com:8443/docs",
        );
        expect(normalizeBrowserAddress("vim keyboard browsing")).toBe(
            "https://duckduckgo.com/?q=vim+keyboard+browsing",
        );
    });

    it("parses navigation, history, lifecycle, and workbench tab commands", () => {
        expect(parseBrowserCommand(":goto wikipedia.org")).toEqual({
            ok: true,
            command: {
                kind: "navigate",
                url: "https://wikipedia.org",
            },
        });
        expect(parseBrowserCommand(":goto vim keyboard browsing")).toEqual({
            ok: true,
            command: {
                kind: "navigate",
                url: "https://duckduckgo.com/?q=vim+keyboard+browsing",
            },
        });
        expect(parseBrowserCommand("back 3")).toEqual({
            ok: true,
            command: { kind: "history", direction: "back", count: 3 },
        });
        expect(parseBrowserCommand("tabprev 2")).toEqual({
            ok: true,
            command: { kind: "select_relative_tab", delta: -2 },
        });
        expect(parseBrowserCommand("tabgoto 4")).toEqual({
            ok: true,
            command: { kind: "select_tab", position: 4 },
        });
        expect(parseBrowserCommand("q")).toEqual({
            ok: true,
            command: { kind: "close" },
        });
        expect(parseBrowserCommand("browser")).toEqual({
            ok: true,
            command: { kind: "open_tab" },
        });
    });

    it("rejects editor-only, ambiguous, and out-of-range commands clearly", () => {
        expect(parseBrowserCommand("w")).toEqual({
            ok: false,
            message:
                ":write is unavailable in a browser tab; use :q to close it",
        });
        expect(parseBrowserCommand("navigate 4")).toEqual({
            ok: false,
            message: "Not a browser command: navigate",
        });
        expect(parseBrowserCommand("back 0").ok).toBe(false);
        expect(parseBrowserCommand("tabgoto nope").ok).toBe(false);
        expect(parseBrowserCommand("browser example.com")).toEqual({
            ok: false,
            message: ":browser does not take arguments",
        });
    });
});
