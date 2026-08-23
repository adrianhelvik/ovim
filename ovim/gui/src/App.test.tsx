/** @vitest-environment jsdom */

import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App, {
    ChatActivityGroup,
    ChatComposer,
    ChatMessageView,
    ChatPanel,
    ChatSetupCard,
    CodeWalkthrough,
    Markdown,
    activitySummary,
    chatSelectionText,
    chatTranscriptItems,
    guiKeyInput,
    imageExtension,
    isNearChatBottom,
    toolResultSummary,
} from "./App";
import { mockSnapshot } from "./mock";
import type { GuiAiChat, GuiCodeExplanation } from "./types";

class ResizeObserverMock {
    observe() {}
    unobserve() {}
    disconnect() {}
}

beforeEach(() => {
    vi.stubGlobal("ResizeObserver", ResizeObserverMock);
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
        callback(0);
        return 1;
    });
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue({
        font: "",
        measureText: () => ({ width: 8 }),
    } as unknown as CanvasRenderingContext2D);
    vi.spyOn(HTMLElement.prototype, "focus").mockImplementation(() => {});
});

afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    document.body.replaceChildren();
});

describe("Ovim Solid workbench", () => {
    it("renders an interactive concept walkthrough from projected core state", () => {
        const onKey = vi.fn();
        const walkthrough: GuiCodeExplanation = {
            current: 1,
            total: 2,
            page: {
                kind: "concept",
                title: "Two layers of history",
                body: "Input recall and conversation navigation are **separate** concerns.",
            },
            discussion: {
                state: "navigating",
                questionCount: 0,
                latestFailed: false,
            },
        };

        render(() => (
            <CodeWalkthrough walkthrough={walkthrough} onKey={onKey} />
        ));

        expect(
            screen.getByRole("dialog", { name: "Two layers of history" }),
        ).toBeTruthy();
        expect(screen.getByText("separate").tagName).toBe("STRONG");
        fireEvent.click(screen.getByRole("button", { name: "Next" }));
        fireEvent.click(screen.getByRole("button", { name: "Ask a question" }));
        expect(onKey).toHaveBeenNthCalledWith(1, "ArrowRight");
        expect(onKey).toHaveBeenNthCalledWith(2, " ");
    });

    it("renders code location and live answer state", () => {
        const walkthrough: GuiCodeExplanation = {
            current: 2,
            total: 2,
            page: {
                kind: "code",
                path: "src/main.rs",
                startLine: 12,
                endLine: 14,
                comment: "This branch owns the handoff.",
            },
            discussion: {
                state: "answering",
                question: "Why here?",
                answer: "Because it owns the boundary.",
                questionCount: 1,
            },
        };

        render(() => (
            <CodeWalkthrough walkthrough={walkthrough} onKey={() => {}} />
        ));

        expect(
            screen.getByRole("dialog", { name: "src/main.rs:12–14" }),
        ).toBeTruthy();
        expect(screen.getByLabelText("Answering")).toBeTruthy();
        expect(screen.getByText("Because it owns the boundary.")).toBeTruthy();
        expect(
            screen
                .getByRole("button", { name: "Finish" })
                .hasAttribute("disabled"),
        ).toBe(true);
    });

    it("renders a keyboard-accessible editor projection from the snapshot", async () => {
        const result = render(() => <App />);

        expect(
            screen.getByRole("navigation", { name: "Primary navigation" }),
        ).toBeTruthy();
        expect(screen.getByRole("button", { name: "Close" })).toBeTruthy();
        expect(screen.getByLabelText("Ovim editor input")).toBeTruthy();
        expect(
            screen.getByRole("tablist", { name: "Open files" }),
        ).toBeTruthy();
        expect(
            screen.getByRole("tree", { name: "Project files" }),
        ).toBeTruthy();
        expect(
            result.container.querySelectorAll(".code-line").length,
        ).toBeGreaterThan(10);
        expect(
            result.container.querySelector(".code-segment.cursor"),
        ).toBeTruthy();

        const focus = vi.mocked(HTMLElement.prototype.focus);
        focus.mockClear();
        fireEvent.mouseDown(result.container.querySelector(".line-content")!, {
            clientX: 80,
        });
        expect(focus.mock.instances).toContain(
            screen.getByLabelText("Ovim editor input"),
        );

        focus.mockClear();
        fireEvent.click(screen.getByRole("tab", { name: "FRONTEND_API.md" }));
        await Promise.resolve();
        expect(focus.mock.instances).toContain(
            screen.getByLabelText("Ovim editor input"),
        );
    });

    it("switches existing compact docks without toggling their core state", () => {
        const previousChat = mockSnapshot.aiChat;
        mockSnapshot.aiChat = {
            profile: "codex",
            profiles: [],
            reasoningEffort: "medium",
            reasoningEffortSelection: "default",
            reasoningEfforts: ["default"],
            activity: "idle",
            waiting: false,
            input: "",
            inputCursor: 0,
            pendingImages: [],
            queuedInputs: [],
            messages: [],
            thinkingLive: false,
            focus: "textInput",
            agents: [],
            agentCursor: 0,
        };
        vi.stubGlobal(
            "matchMedia",
            vi.fn(() => ({
                matches: true,
                addEventListener: vi.fn(),
                removeEventListener: vi.fn(),
            })),
        );

        const result = render(() => <App />);
        try {
            const workbench = result.container.querySelector(".workbench")!;
            expect(workbench.classList).toContain("active-context-dock");

            fireEvent.click(screen.getByRole("button", { name: "Explorer" }));
            expect(workbench.classList).toContain("active-explorer-dock");
            expect(mockSnapshot.fileTree).toBeTruthy();

            fireEvent.click(screen.getByRole("button", { name: "AI chat" }));
            expect(workbench.classList).toContain("active-context-dock");
            expect(mockSnapshot.aiChat).toBeTruthy();
        } finally {
            result.unmount();
            mockSnapshot.aiChat = previousChat;
        }
    });

    it("sanitizes rendered AI markdown", () => {
        const result = render(() => (
            <Markdown
                text={'**safe**<img src="x" onerror="window.__unsafe = true">'}
            />
        ));

        expect(screen.getByText("safe").tagName).toBe("STRONG");
        expect(
            result.container.querySelector("img")?.hasAttribute("onerror"),
        ).toBe(false);
        expect(result.container.querySelector("script")).toBeNull();
    });

    it("hands safe markdown links to the desktop opener without navigating the editor", () => {
        const onOpenLink = vi.fn();
        const result = render(() => (
            <Markdown
                text="Read the [Ovim guide](https://example.com/guide)."
                onOpenLink={onOpenLink}
            />
        ));
        const link = screen.getByRole("link", { name: "Ovim guide" });
        const click = new MouseEvent("click", {
            bubbles: true,
            cancelable: true,
        });

        link.dispatchEvent(click);

        expect(click.defaultPrevented).toBe(true);
        expect(onOpenLink).toHaveBeenCalledWith("https://example.com/guide");
        expect(result.container.ownerDocument.location.href).toBe(
            "http://localhost:3000/",
        );
    });

    it("uses a native textarea while preserving UTF-8 core cursor offsets", async () => {
        const onUpdate = vi.fn().mockResolvedValue(undefined);
        render(() => (
            <ChatComposer
                onUpdate={onUpdate}
                chat={{
                    profile: "codex",
                    profiles: [
                        { id: "codex", provider: "codex", model: "gpt-test" },
                    ],
                    reasoningEffort: "high",
                    reasoningEffortSelection: "default",
                    reasoningEfforts: ["default", "high"],
                    activity: "idle",
                    waiting: false,
                    input: "a界b",
                    inputCursor: 4,
                    queuedInputs: [],
                    pendingImages: ["diagram.png"],
                    messages: [],
                    thinkingLive: false,
                    focus: "textInput",
                    agents: [],
                    agentCursor: 0,
                }}
            />
        ));

        expect(screen.getByText("diagram.png")).toBeTruthy();
        const input = screen.getByLabelText(
            "AI chat input",
        ) as HTMLTextAreaElement;
        await Promise.resolve();
        expect(input.value).toBe("a界b");
        expect(input.selectionStart).toBe(2);

        input.value = "a›界b";
        input.setSelectionRange(2, 2);
        fireEvent.input(input);
        await Promise.resolve();
        expect(onUpdate).toHaveBeenCalledWith({
            expectedInput: "a界b",
            expectedCursor: 4,
            input: "a›界b",
            cursor: 4,
            action: undefined,
        });
    });

    it("publishes submit atomically with the latest native draft", async () => {
        const onUpdate = vi.fn().mockResolvedValue(undefined);
        render(() => (
            <ChatComposer
                onUpdate={onUpdate}
                chat={{
                    profile: "codex",
                    profiles: [],
                    reasoningEffort: "medium",
                    reasoningEffortSelection: "default",
                    reasoningEfforts: ["default"],
                    activity: "idle",
                    waiting: false,
                    input: "",
                    inputCursor: 0,
                    queuedInputs: [],
                    pendingImages: [],
                    messages: [],
                    thinkingLive: false,
                    focus: "textInput",
                    agents: [],
                    agentCursor: 0,
                }}
            />
        ));
        const input = screen.getByLabelText(
            "AI chat input",
        ) as HTMLTextAreaElement;
        input.value = "ship this";
        input.setSelectionRange(9, 9);
        fireEvent.input(input);
        await Promise.resolve();
        input.setSelectionRange(9, 9);
        fireEvent.keyDown(input, { key: "Enter" });
        await waitFor(() => expect(onUpdate).toHaveBeenCalledTimes(2));

        expect(onUpdate).toHaveBeenLastCalledWith(
            expect.objectContaining({
                expectedInput: "ship this",
                input: "ship this",
                cursor: 9,
                action: expect.objectContaining({ key: "Enter" }),
            }),
        );
        expect(input.value).toBe("");
    });

    it("returns DOM focus to the editor input when AI chat is activated", () => {
        render(() => <App />);
        const input = screen.getByLabelText("Ovim editor input");
        const focus = vi.mocked(HTMLElement.prototype.focus);
        focus.mockClear();

        fireEvent.click(
            document.querySelector<HTMLButtonElement>('[title^="AI chat"]')!,
        );

        expect(focus).toHaveBeenCalledWith({ preventScroll: true });
        expect(focus.mock.instances).toContain(input);
    });

    it("follows chat updates until the reader scrolls away from the bottom", async () => {
        const initial: GuiAiChat = {
            profile: "codex",
            profiles: [{ id: "codex", provider: "codex", model: "gpt-test" }],
            reasoningEffort: "high",
            reasoningEffortSelection: "default",
            reasoningEfforts: ["default", "high"],
            activity: "idle",
            waiting: false,
            input: "",
            inputCursor: 0,
            queuedInputs: [],
            pendingImages: [],
            messages: [
                {
                    id: "1:1",
                    index: 0,
                    selected: false,
                    role: "assistant",
                    content: "First response",
                    model: "codex",
                    tools: [],
                },
            ],
            thinkingLive: false,
            focus: "textInput",
            agents: [],
            agentCursor: 0,
        };
        const [chat, setChat] = createSignal(initial);
        const result = render(() => (
            <ChatPanel chat={chat()} focusInput={() => {}} />
        ));
        const transcript =
            result.container.querySelector<HTMLElement>(".chat-messages")!;
        Object.defineProperties(transcript, {
            scrollHeight: { configurable: true, value: 600, writable: true },
            clientHeight: { configurable: true, value: 200 },
            scrollTop: { configurable: true, value: 0, writable: true },
        });

        await Promise.resolve();
        expect(transcript.scrollTop).toBe(600);

        transcript.scrollTop = 360;
        fireEvent.scroll(transcript);
        Object.defineProperty(transcript, "scrollHeight", {
            configurable: true,
            value: 700,
        });
        setChat({ ...initial, streaming: "Streaming while pinned" });
        await Promise.resolve();
        expect(transcript.scrollTop).toBe(700);

        transcript.scrollTop = 100;
        fireEvent.scroll(transcript);
        expect(
            screen.getByRole("button", { name: "New messages" }),
        ).toBeTruthy();
        setChat({ ...initial, streaming: "More streaming content" });
        await Promise.resolve();
        expect(transcript.scrollTop).toBe(100);

        fireEvent.click(screen.getByRole("button", { name: "New messages" }));
        expect(transcript.scrollTop).toBe(700);
    });

    it("does not repeatedly pull the reader back to an unchanged history selection", async () => {
        const selectedMessage: GuiAiChat["messages"][number] = {
            id: "1:1",
            index: 0,
            selected: true,
            role: "assistant",
            content: "Pinned response",
            model: "codex",
            tools: [],
        };
        const initial: GuiAiChat = {
            profile: "codex",
            profiles: [],
            reasoningEffort: "medium",
            reasoningEffortSelection: "default",
            reasoningEfforts: ["default"],
            activity: "idle",
            waiting: false,
            input: "",
            inputCursor: 0,
            queuedInputs: [],
            pendingImages: [],
            messages: [selectedMessage],
            thinkingLive: false,
            focus: "textInput",
            agents: [],
            agentCursor: 0,
        };
        const [chat, setChat] = createSignal(initial);
        const result = render(() => (
            <ChatPanel chat={chat()} focusInput={() => {}} />
        ));
        const transcript =
            result.container.querySelector<HTMLElement>(".chat-messages")!;
        Object.defineProperties(transcript, {
            scrollHeight: { configurable: true, value: 600, writable: true },
            clientHeight: { configurable: true, value: 200 },
            scrollTop: { configurable: true, value: 100, writable: true },
        });

        await Promise.resolve();
        fireEvent.click(screen.getByRole("button", { name: "New messages" }));
        expect(transcript.scrollTop).toBe(600);

        setChat({
            ...initial,
            activity: "streaming",
            streaming: "New content on the same selected turn",
        });
        await Promise.resolve();

        expect(transcript.scrollTop).toBe(600);
        expect(
            screen.queryByRole("button", { name: "New activity" }),
        ).toBeNull();
    });

    it("offers configured model selections and releases pointer focus back to chat input", async () => {
        const onProfile = vi.fn();
        const onReasoningEffort = vi.fn();
        const onQueuedAction = vi.fn();
        const focusInput = vi.fn();
        const chat: GuiAiChat = {
            profile: "codex",
            profiles: [
                { id: "codex", provider: "codex", model: "gpt-test" },
                { id: "local", provider: "ollama", model: "qwen-test" },
            ],
            reasoningEffort: "medium",
            reasoningEffortSelection: "default",
            reasoningEfforts: ["default", "low", "medium", "high"],
            activity: "idle",
            waiting: false,
            input: "preserved",
            inputCursor: 9,
            queuedInputs: [
                {
                    id: 7,
                    kind: "followUp",
                    content: "Check the remaining tests",
                    imageCount: 1,
                    hasCodeAttachment: true,
                    selected: false,
                },
            ],
            pendingImages: [],
            messages: [],
            thinkingLive: false,
            focus: "textInput",
            agents: [],
            agentCursor: 0,
        };
        render(() => (
            <ChatPanel
                chat={chat}
                focusInput={focusInput}
                onProfile={onProfile}
                onReasoningEffort={onReasoningEffort}
                onQueuedAction={onQueuedAction}
            />
        ));

        const trigger = screen.getByRole("button", {
            name: /codex.*default.*medium/i,
        });
        fireEvent.click(trigger);
        const search = screen.getByLabelText("Model profile");
        const focus = vi.mocked(HTMLElement.prototype.focus);
        focus.mockClear();
        fireEvent.keyDown(search, { key: "ArrowDown" });
        await Promise.resolve();
        expect(focus.mock.instances).toContain(
            screen.getByRole("option", { name: /codex.*gpt-test/i }),
        );
        fireEvent.input(search, { target: { value: "qwen" } });
        expect(screen.queryByRole("option", { name: /codex/i })).toBeNull();
        fireEvent.click(
            screen.getByRole("option", { name: /local.*ollama.*qwen-test/i }),
        );
        await Promise.resolve();
        expect(onProfile).toHaveBeenCalledWith("local");
        expect(focusInput).toHaveBeenCalledOnce();

        fireEvent.click(trigger);
        fireEvent.click(screen.getByRole("button", { name: "high" }));
        await Promise.resolve();
        expect(onReasoningEffort).toHaveBeenCalledWith("high");
        expect(screen.getByText("Queued message")).toBeTruthy();
        expect(screen.getByText("Check the remaining tests")).toBeTruthy();
        expect(screen.getByText("1 image")).toBeTruthy();
        expect(screen.getByText("code attached")).toBeTruthy();
        fireEvent.click(screen.getByText("Check the remaining tests"));
        fireEvent.click(screen.getByRole("button", { name: "Edit" }));
        fireEvent.click(screen.getByRole("button", { name: "Remove" }));
        expect(onQueuedAction.mock.calls).toEqual([
            [7, "select"],
            [7, "recall"],
            [7, "remove"],
        ]);
    });

    it("exposes send, stop, and attachment removal as working controls", async () => {
        const onUpdate = vi.fn().mockResolvedValue(undefined);
        const onRemoveImage = vi.fn();
        const chat: GuiAiChat = {
            profile: "codex",
            profiles: [],
            reasoningEffort: "medium",
            reasoningEffortSelection: "default",
            reasoningEfforts: ["default"],
            activity: "idle",
            waiting: false,
            input: "ship this",
            inputCursor: 9,
            queuedInputs: [],
            pendingImages: ["diagram.png"],
            messages: [],
            thinkingLive: false,
            focus: "textInput",
            agents: [],
            agentCursor: 0,
        };
        const result = render(() => (
            <ChatComposer
                chat={chat}
                onUpdate={onUpdate}
                onRemoveImage={onRemoveImage}
            />
        ));

        fireEvent.click(
            screen.getByRole("button", { name: "Remove diagram.png" }),
        );
        expect(onRemoveImage).toHaveBeenCalledWith(0);
        fireEvent.click(screen.getByRole("button", { name: "Send message" }));
        await waitFor(() => expect(onUpdate).toHaveBeenCalled());
        expect(onUpdate).toHaveBeenLastCalledWith(
            expect.objectContaining({
                input: "ship this",
                action: expect.objectContaining({ key: "Enter" }),
            }),
        );
        result.unmount();

        const onStop = vi.fn().mockResolvedValue(undefined);
        render(() => (
            <ChatComposer
                chat={{ ...chat, activity: "streaming", waiting: true }}
                onUpdate={onStop}
            />
        ));
        fireEvent.click(
            screen.getByRole("button", { name: "Stop generation" }),
        );
        await waitFor(() => expect(onStop).toHaveBeenCalled());
        expect(onStop).toHaveBeenLastCalledWith(
            expect.objectContaining({
                action: expect.objectContaining({ key: "Escape" }),
            }),
        );
    });

    it("shows and activates core history and agent navigation state", () => {
        const onMessage = vi.fn();
        const onAgent = vi.fn();
        const chat: GuiAiChat = {
            profile: "codex",
            profiles: [{ id: "codex", provider: "codex", model: "gpt-test" }],
            reasoningEffort: "high",
            reasoningEffortSelection: "default",
            reasoningEfforts: ["default", "high"],
            activity: "idle",
            waiting: false,
            input: "",
            inputCursor: 0,
            queuedInputs: [],
            pendingImages: [],
            thinkingLive: false,
            focus: "treePanel",
            agentCursor: 1,
            selectedAgentId: "agt_1",
            agents: [
                {
                    id: "agt_1",
                    taskName: "Review changes",
                    lifecycle: "running",
                    model: "gpt-test",
                    depth: 0,
                },
            ],
            messages: [
                {
                    id: "1:1",
                    index: 0,
                    selected: true,
                    role: "assistant",
                    content: "Selected response",
                    tools: [],
                },
            ],
        };
        const result = render(() => (
            <ChatPanel
                chat={chat}
                focusInput={() => {}}
                onMessage={onMessage}
                onAgent={onAgent}
            />
        ));

        expect(
            result.container.querySelector(".chat-message.selected"),
        ).toBeTruthy();
        expect(
            screen
                .getByRole("button", { name: /Review changes/ })
                .classList.contains("cursor"),
        ).toBe(true);
        fireEvent.click(screen.getByText("Selected response"));
        fireEvent.click(screen.getByRole("button", { name: /Review changes/ }));
        expect(onMessage).toHaveBeenCalledWith(0);
        expect(onAgent).toHaveBeenCalledWith("agt_1");
    });

    it("uses a small threshold when deciding whether chat should follow", () => {
        expect(
            isNearChatBottom({
                scrollHeight: 500,
                scrollTop: 260,
                clientHeight: 200,
            }),
        ).toBe(true);
        expect(
            isNearChatBottom({
                scrollHeight: 500,
                scrollTop: 200,
                clientHeight: 200,
            }),
        ).toBe(false);
    });

    it("does not let stale editor overlays cover an active AI chat", () => {
        mockSnapshot.aiChat = {
            profile: "codex",
            profiles: [{ id: "codex", provider: "codex", model: "gpt-test" }],
            reasoningEffort: "high",
            reasoningEffortSelection: "default",
            reasoningEfforts: ["default", "high"],
            activity: "idle",
            waiting: false,
            input: "visible draft",
            inputCursor: 13,
            queuedInputs: [],
            pendingImages: [],
            messages: [],
            thinkingLive: false,
            focus: "textInput",
            agents: [],
            agentCursor: 0,
        };
        mockSnapshot.picker = {
            title: "Stale picker",
            query: "",
            selected: 0,
            total: 1,
            items: [
                {
                    index: 0,
                    display: "Result",
                    location: "src/main.rs",
                    matched: [],
                },
            ],
        };
        mockSnapshot.lspManager = {
            filter: "",
            selected: 0,
            showDetail: false,
            items: [],
        };
        mockSnapshot.hover = { content: "Stale hover" };
        mockSnapshot.completion = {
            selected: 0,
            items: [{ index: 0, label: "stale" }],
        };

        try {
            const result = render(() => <App />);
            expect(
                (screen.getByLabelText("AI chat input") as HTMLTextAreaElement)
                    .value,
            ).toContain("visible draft");
            expect(result.container.querySelector(".overlay-shade")).toBeNull();
            expect(result.container.querySelector(".hover-popover")).toBeNull();
            expect(
                result.container.querySelector(".completion-popover"),
            ).toBeNull();
        } finally {
            delete mockSnapshot.aiChat;
            delete mockSnapshot.picker;
            delete mockSnapshot.lspManager;
            delete mockSnapshot.hover;
            delete mockSnapshot.completion;
        }
    });

    it("shows blocking chat setup inline with masked input and working actions", () => {
        const onKey = vi.fn();
        render(() => (
            <ChatSetupCard
                setup={{
                    kind: "exaKey",
                    title: "Enable web search",
                    detail: "Paste an Exa API key or skip this optional setup.",
                    maskedInput: "••••",
                    inputCursor: 2,
                    actions: [
                        { label: "Save key", key: "Enter" },
                        { label: "Not now", key: "Escape" },
                    ],
                }}
                onKey={onKey}
            />
        ));

        const input = screen.getByLabelText("Exa API key input");
        expect(input.textContent).toBe("••••");
        expect(
            input.querySelector(".chat-caret")?.previousSibling?.textContent,
        ).toBe("••");
        fireEvent.click(screen.getByRole("button", { name: "Not now" }));
        expect(onKey).toHaveBeenCalledWith("Escape");
    });

    it("recognizes a browser selection made inside the chat transcript", () => {
        const transcript = document.createElement("div");
        transcript.className = "chat-messages";
        transcript.textContent = "copy this response";
        document.body.append(transcript);
        const range = document.createRange();
        range.selectNodeContents(transcript);
        const selection = window.getSelection()!;
        selection.removeAllRanges();
        selection.addRange(range);

        expect(chatSelectionText(selection)).toBe("copy this response");

        transcript.className = "editor-content";
        expect(chatSelectionText(selection)).toBe("");
    });

    it("accepts only image formats supported by the core attachment path", () => {
        expect(imageExtension("image/png")).toBe("png");
        expect(imageExtension("image/jpeg")).toBe("jpg");
        expect(imageExtension("image/gif")).toBe("gif");
        expect(imageExtension("image/webp")).toBe("webp");
        expect(imageExtension("image/svg+xml")).toBeUndefined();
    });

    it("ignores standalone modifiers and treats option-produced Unicode as text", () => {
        expect(
            guiKeyInput({
                key: "Shift",
                shiftKey: true,
                ctrlKey: false,
                altKey: false,
                metaKey: false,
            }),
        ).toBeUndefined();
        expect(
            guiKeyInput({
                key: "›",
                shiftKey: true,
                ctrlKey: false,
                altKey: true,
                metaKey: false,
            }),
        ).toEqual({
            key: "›",
            shift: true,
            control: false,
            alt: false,
            meta: false,
        });
        expect(
            guiKeyInput({
                key: "b",
                shiftKey: false,
                ctrlKey: false,
                altKey: true,
                metaKey: false,
            })?.alt,
        ).toBe(true);
    });

    it("groups contiguous thinking and tool activity behind one live summary", async () => {
        const items = chatTranscriptItems(
            [
                {
                    id: "1:1",
                    index: 0,
                    selected: false,
                    role: "user",
                    content: "Please inspect this",
                    tools: [],
                },
                {
                    id: "1:2",
                    index: 1,
                    selected: false,
                    role: "thinking",
                    content: "**Planning the inspection**",
                    model: "codex",
                    tools: [],
                },
                {
                    id: "1:3",
                    index: 2,
                    selected: false,
                    role: "assistant",
                    content: "",
                    model: "codex",
                    tools: ["search_project"],
                },
                {
                    id: "1:4",
                    index: 3,
                    selected: false,
                    role: "tool",
                    content: "Found three matches",
                    toolName: "search_project",
                    tools: [],
                },
                {
                    id: "1:5",
                    index: 4,
                    selected: false,
                    role: "assistant",
                    content: "Here is the result",
                    model: "codex",
                    tools: [],
                },
            ],
            "Inspecting the matching files",
            true,
        );

        expect(items.map((item) => item.kind)).toEqual([
            "message",
            "activity",
            "message",
            "activity",
        ]);
        const live = items.at(-1)!;
        expect(live.kind).toBe("activity");
        if (live.kind !== "activity") throw new Error("expected live activity");
        expect(activitySummary(live.entries)).toBe(
            "Inspecting the matching files",
        );

        const result = render(() => <ChatActivityGroup item={live} />);
        expect(screen.getByText("Inspecting the matching files")).toBeTruthy();
        expect(screen.getByLabelText("Working")).toBeTruthy();
        expect(
            result.container.querySelector(".chat-activity-history"),
        ).toBeNull();
        const details =
            result.container.querySelector<HTMLDetailsElement>("details")!;
        details.open = true;
        fireEvent(details, new Event("toggle"));
        await Promise.resolve();
        expect(
            result.container.querySelector(".chat-activity-history"),
        ).toBeTruthy();
    });

    it("keeps the latest activity live between thinking chunks while work continues", () => {
        const items = chatTranscriptItems(
            [
                {
                    id: "1:1",
                    index: 0,
                    selected: false,
                    role: "thinking",
                    content: "**Planning**",
                    tools: [],
                },
                {
                    id: "1:2",
                    index: 1,
                    selected: false,
                    role: "assistant",
                    content: "",
                    tools: ["search_project"],
                },
                {
                    id: "1:3",
                    index: 2,
                    selected: false,
                    role: "tool",
                    content: "Found matches",
                    toolName: "search_project",
                    tools: [],
                },
            ],
            undefined,
            false,
            true,
        );

        const activity = items.at(-1);
        expect(activity?.kind).toBe("activity");
        if (activity?.kind !== "activity") throw new Error("expected activity");
        expect(activity.live).toBe(true);
        expect(activitySummary(activity.entries)).toBe("Planning");
    });

    it("keeps one activity group across tool-bearing assistant commentary", () => {
        const items = chatTranscriptItems(
            [
                {
                    id: "1:1",
                    index: 0,
                    selected: false,
                    role: "thinking",
                    content: "Planning",
                    tools: [],
                },
                {
                    id: "1:2",
                    index: 1,
                    selected: false,
                    role: "assistant",
                    content: "I’ll inspect the matching files.",
                    tools: ["search_project"],
                },
                {
                    id: "1:3",
                    index: 2,
                    selected: false,
                    role: "tool",
                    content: "Found matches",
                    toolName: "search_project",
                    tools: [],
                },
            ],
            "Comparing results",
            true,
            true,
        );
        expect(items.map((item) => item.kind)).toEqual(["activity", "message"]);
        const activity = items[0];
        if (activity.kind !== "activity") throw new Error("expected activity");
        expect(activity.entries.map((entry) => entry.role)).toEqual([
            "thinking",
            "tool",
            "thinking",
        ]);
        expect(activity.live).toBe(true);
    });

    it("keeps tool results collapsed until their details are requested", async () => {
        const payload = "large tool payload that should start hidden";
        const result = render(() => (
            <ChatMessageView
                message={{
                    id: "1:3",
                    index: 2,
                    selected: false,
                    role: "tool",
                    content: payload,
                    toolName: "search_project",
                    tools: [],
                }}
            />
        ));

        expect(screen.getByText("search_project")).toBeTruthy();
        expect(screen.getByText(toolResultSummary(payload))).toBeTruthy();
        expect(result.container.querySelector(".markdown")).toBeNull();

        const details =
            result.container.querySelector<HTMLDetailsElement>("details")!;
        details.open = true;
        fireEvent(details, new Event("toggle"));
        await Promise.resolve();
        expect(screen.getByText(payload)).toBeTruthy();
    });

    it("collapses assistant tool-call lists by default", () => {
        const result = render(() => (
            <ChatMessageView
                message={{
                    id: "1:4",
                    index: 3,
                    selected: false,
                    role: "assistant",
                    content: "I will inspect this.",
                    model: "codex",
                    tools: ["search_project", "read_file_at_path"],
                }}
            />
        ));

        const details =
            result.container.querySelector<HTMLDetailsElement>(
                ".tool-call-list",
            )!;
        expect(details.open).toBe(false);
        expect(screen.getByText("2 tool calls")).toBeTruthy();
    });
});
