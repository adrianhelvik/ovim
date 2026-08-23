import { describe, expect, it } from "vitest";
import {
    readWorkbenchLayout,
    workspaceLayoutIdentity,
    writeWorkbenchLayout,
} from "./layoutPersistence";

describe("workspace layout persistence", () => {
    it("derives a stable workspace path on Unix and Windows", () => {
        expect(
            workspaceLayoutIdentity({
                filePath: "/work/ovim/ovim/src/gui/mod.rs",
                workspacePath: "/work/ovim",
                projectName: "ovim",
            }),
        ).toBe("/work/ovim");
        expect(
            workspaceLayoutIdentity({
                filePath: "C:\\work\\ovim\\src\\main.rs",
                workspacePath: "C:\\work\\ovim",
                projectName: "ovim",
            }),
        ).toBe("C:/work/ovim");
    });

    it("round-trips valid preferences and ignores corrupt storage", () => {
        const values = new Map<string, string>();
        const storage = {
            getItem: (key: string) => values.get(key) ?? null,
            setItem: (key: string, value: string) => values.set(key, value),
        };
        const preference = {
            activeDock: "context" as const,
            activeContextPanel: "debug" as const,
        };

        writeWorkbenchLayout(storage, "/work/ovim", preference);
        expect(readWorkbenchLayout(storage, "/work/ovim")).toEqual(preference);

        values.set("ovim.gui.layout.v1.%2Fwork%2Fbroken", "not json");
        expect(readWorkbenchLayout(storage, "/work/broken")).toBeUndefined();
    });
});
