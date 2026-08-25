import {
    For,
    Index,
    Show,
    createEffect,
    createMemo,
    createSignal,
    onCleanup,
    onMount,
} from "solid-js";
import { Channel, invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import DOMPurify from "dompurify";
import { marked } from "marked";
import { mockSnapshot } from "./mock";
import ChatModelPicker from "./ChatModelPicker";
import ChatComposer, { type ChatInputUpdate } from "./ChatComposer";
import BrowserPanel, {
    browserTabTitle,
    type BrowserSession,
    type BrowserState,
} from "./BrowserPanel";
import ContextDock, { type ContextPanelDefinition } from "./ContextDock";
import GdiffPanel from "./GdiffPanel";
import { guiKeyInput } from "./guiInput";
import { Icon, IconButton, type IconTone } from "./Icon";
import { themeVariables } from "./theme";
import { splitAtUtf8Offset } from "./textEncoding";
import { isGuiNativeControl, trapDialogFocus } from "./focus";
import { anchoredOverlayPosition } from "./overlayPosition";
import { retainProjection, shouldAcceptRevision } from "./stateProjection";
import {
    readWorkbenchLayout,
    workspaceLayoutIdentity,
    writeWorkbenchLayout,
} from "./layoutPersistence";
import type {
    GuiAiChat,
    GuiCodeExplanation,
    GuiGdiffReview,
    GuiKeyInput,
    GuiLayoutNode,
    GuiPane,
    GuiSnapshot,
} from "./types";

export { default as ChatComposer } from "./ChatComposer";
export { guiKeyInput } from "./guiInput";

const LINE_HEIGHT = 22;
const FALLBACK_CELL_WIDTH = 8.15;
const MAX_IMAGE_BYTES = 20 * 1024 * 1024;

interface VectorPreview {
    dataUrl: string;
    width: number;
    height: number;
    fileName: string;
}

const openExternalLink = (url: string) => {
    if (!/^(https?:\/\/|mailto:)/i.test(url)) return;
    if (isTauri()) {
        void invoke("gui_open_external", { url }).catch(() => {});
        return;
    }
    window.open(url, "_blank", "noopener,noreferrer");
};

export const Markdown = (props: {
    text: string;
    onOpenLink?: (url: string) => void;
}) => {
    const html = createMemo(() =>
        DOMPurify.sanitize(
            marked.parse(props.text, {
                async: false,
                breaks: true,
                gfm: true,
            }) as string,
            { USE_PROFILES: { html: true } },
        ),
    );

    return (
        <div
            class="markdown"
            innerHTML={html()}
            onClick={(event) => {
                const link = (event.target as Element).closest("a[href]");
                if (!link || !event.currentTarget.contains(link)) return;
                event.preventDefault();
                const url = link.getAttribute("href") ?? "";
                (props.onOpenLink ?? openExternalLink)(url);
            }}
        />
    );
};

export const QueuedChatMessage = (props: {
    item: GuiAiChat["queuedInputs"][number];
    onAction?: (id: number, action: "select" | "recall" | "remove") => void;
}) => {
    const label = () =>
        props.item.kind === "steer"
            ? ["Queued steer", "next tool boundary"]
            : props.item.kind === "command"
              ? ["Queued command", "after this round"]
              : ["Queued message", "next round"];
    return (
        <article
            class="chat-message user queued"
            classList={{ selected: props.item.selected }}
            aria-current={props.item.selected ? "true" : undefined}
            onClick={() => props.onAction?.(props.item.id, "select")}
        >
            <header>
                <b>{label()[0]}</b>
                <small>{label()[1]}</small>
            </header>
            <Show when={props.item.content}>
                <Markdown text={props.item.content} />
            </Show>
            <Show when={props.item.imageCount || props.item.hasCodeAttachment}>
                <footer class="queued-attachments">
                    <Show when={props.item.imageCount}>
                        <span>
                            {props.item.imageCount}{" "}
                            {props.item.imageCount === 1 ? "image" : "images"}
                        </span>
                    </Show>
                    <Show when={props.item.hasCodeAttachment}>
                        <span>code attached</span>
                    </Show>
                </footer>
            </Show>
            <footer class="queued-actions">
                <button
                    type="button"
                    onClick={(event) => {
                        event.stopPropagation();
                        props.onAction?.(props.item.id, "recall");
                    }}
                >
                    Edit
                </button>
                <button
                    type="button"
                    onClick={(event) => {
                        event.stopPropagation();
                        props.onAction?.(props.item.id, "remove");
                    }}
                >
                    Remove
                </button>
            </footer>
        </article>
    );
};

export const ChatActivityGroup = (props: {
    item: Extract<ChatTranscriptItem, { kind: "activity" }>;
    onSelect?: (index: number) => void;
}) => {
    const [expanded, setExpanded] = createSignal(false);
    return (
        <details
            class="chat-activity"
            classList={{
                live: props.item.live,
                selected: props.item.entries.some((entry) => entry.selected),
            }}
            data-selected={
                props.item.entries.some((entry) => entry.selected) || undefined
            }
            aria-current={
                props.item.entries.some((entry) => entry.selected)
                    ? "true"
                    : undefined
            }
            onToggle={(event) => setExpanded(event.currentTarget.open)}
        >
            <summary>
                <Icon name="chevron-right" size={16} />
                <span
                    classList={{ "thinking-spinner": props.item.live }}
                    aria-label={props.item.live ? "Working" : undefined}
                />
                <span>
                    <small>{props.item.live ? "Working" : "Activity"}</small>
                    <b>{activitySummary(props.item.entries)}</b>
                </span>
                <em>
                    {props.item.entries.length}{" "}
                    {props.item.entries.length === 1 ? "step" : "steps"}
                </em>
            </summary>
            <Show when={expanded()}>
                <div class="chat-activity-history">
                    <For each={props.item.entries}>
                        {(entry) => (
                            <section
                                class={`chat-activity-entry ${entry.role}`}
                                onClick={() => props.onSelect?.(entry.index)}
                            >
                                <header>
                                    <b>
                                        {entry.role === "tool"
                                            ? entry.toolName || "Tool result"
                                            : entry.role}
                                    </b>
                                    <small>
                                        {entry.live ? "live" : entry.model}
                                    </small>
                                </header>
                                <Show when={entry.content}>
                                    <Markdown text={entry.content} />
                                </Show>
                                <ToolCallList tools={entry.tools} />
                            </section>
                        )}
                    </For>
                </div>
            </Show>
        </details>
    );
};

const WalkthroughDiscussion = (props: {
    discussion: GuiCodeExplanation["discussion"];
}) => (
    <Show
        when={
            props.discussion.state !== "navigating" ||
            props.discussion.latestQuestion
        }
    >
        <section
            class={`walkthrough-discussion ${props.discussion.state}`}
            aria-live={
                props.discussion.state === "answering" ? "polite" : "off"
            }
        >
            <Show
                when={
                    props.discussion.state === "composing"
                        ? props.discussion
                        : undefined
                }
            >
                {(active) => {
                    const parts = createMemo(() =>
                        splitAtUtf8Offset(active().input, active().cursor),
                    );
                    return (
                        <>
                            <small>Ask about this page</small>
                            <pre class="walkthrough-question">
                                <span>{parts()[0]}</span>
                                <i class="chat-caret" aria-hidden="true" />
                                <span>
                                    {parts()[1] || "Type your question…"}
                                </span>
                            </pre>
                        </>
                    );
                }}
            </Show>
            <Show
                when={
                    props.discussion.state === "answering"
                        ? props.discussion
                        : undefined
                }
            >
                {(active) => (
                    <>
                        <small>Answering “{active().question}”</small>
                        <div class="walkthrough-answer">
                            <span
                                class="walkthrough-spinner"
                                aria-label="Answering"
                            />
                            <Markdown text={active().answer || "Thinking…"} />
                        </div>
                    </>
                )}
            </Show>
            <Show
                when={
                    props.discussion.state === "navigating" &&
                    props.discussion.latestQuestion
                        ? props.discussion
                        : undefined
                }
            >
                {(active) => {
                    return (
                        <>
                            <small>
                                {active().latestFailed
                                    ? "Answer failed"
                                    : `Question ${active().questionCount}`}
                                : {active().latestQuestion}
                            </small>
                            <div class="walkthrough-answer">
                                <Markdown text={active().latestAnswer || ""} />
                            </div>
                        </>
                    );
                }}
            </Show>
        </section>
    </Show>
);

export const CodeWalkthrough = (props: {
    walkthrough: GuiCodeExplanation;
    onKey: (key: string) => void;
    restoreFocus?: () => void;
}) => {
    let dialog!: HTMLElement;
    const dispatch = (key: string) => {
        props.onKey(key);
        queueMicrotask(() => dialog?.focus({ preventScroll: true }));
    };
    onMount(() => queueMicrotask(() => dialog?.focus({ preventScroll: true })));
    onCleanup(() => queueMicrotask(() => props.restoreFocus?.()));
    const page = () => props.walkthrough.page;
    const title = () => {
        const active = page();
        if (active.kind === "concept") return active.title;
        return `${active.path}:${active.startLine}${active.endLine !== active.startLine ? `–${active.endLine}` : ""}`;
    };
    const teaching = () => {
        const active = page();
        return active.kind === "concept" ? active.body : active.comment;
    };
    const composing = () => props.walkthrough.discussion.state === "composing";
    const answering = () => props.walkthrough.discussion.state === "answering";
    return (
        <div
            class={`walkthrough-layer ${page().kind}`}
            aria-label="Code walkthrough"
        >
            <section
                ref={dialog!}
                class="walkthrough-card"
                role="dialog"
                aria-modal="true"
                aria-labelledby="walkthrough-title"
                data-gui-core-dialog
                tabIndex={-1}
                onKeyDown={(event) =>
                    void trapDialogFocus(event, event.currentTarget)
                }
            >
                <header>
                    <div>
                        <small>
                            {page().kind === "concept"
                                ? "Concept"
                                : "Code walkthrough"}{" "}
                            · {props.walkthrough.current} of{" "}
                            {props.walkthrough.total}
                        </small>
                        <b id="walkthrough-title">{title()}</b>
                    </div>
                    <button
                        type="button"
                        aria-label="Dismiss walkthrough"
                        onClick={() => dispatch("Escape")}
                    >
                        Esc
                    </button>
                </header>
                <div class="walkthrough-teaching">
                    <Markdown text={teaching()} />
                </div>
                <WalkthroughDiscussion
                    discussion={props.walkthrough.discussion}
                />
                <footer>
                    <div class="walkthrough-pages">
                        <button
                            type="button"
                            disabled={
                                props.walkthrough.current === 1 || composing()
                            }
                            onClick={() => dispatch("ArrowLeft")}
                        >
                            Previous
                        </button>
                        <button
                            type="button"
                            disabled={
                                props.walkthrough.current ===
                                    props.walkthrough.total || composing()
                            }
                            onClick={() => dispatch("ArrowRight")}
                        >
                            Next
                            <Icon name="chevron-right" size={16} />
                        </button>
                    </div>
                    <div class="walkthrough-actions">
                        <button
                            type="button"
                            disabled={answering()}
                            onClick={() =>
                                dispatch(composing() ? "Escape" : " ")
                            }
                        >
                            {composing() ? "Cancel question" : "Ask a question"}
                        </button>
                        <button
                            type="button"
                            class="primary"
                            disabled={answering()}
                            onClick={() => dispatch("Enter")}
                        >
                            {composing()
                                ? "Send question"
                                : props.walkthrough.current ===
                                    props.walkthrough.total
                                  ? "Finish"
                                  : "Continue"}
                        </button>
                    </div>
                </footer>
            </section>
        </div>
    );
};

export const imageExtension = (mimeType: string) =>
    (
        ({
            "image/png": "png",
            "image/jpeg": "jpg",
            "image/gif": "gif",
            "image/webp": "webp",
        }) as Record<string, string>
    )[mimeType];

export const chatSelectionText = (
    selection: Selection | null = window.getSelection(),
) => {
    if (!selection || selection.isCollapsed || !selection.rangeCount) return "";
    const elementFor = (node: Node | null) =>
        node instanceof Element ? node : node?.parentElement;
    const anchorChat = elementFor(selection.anchorNode)?.closest(
        ".chat-messages",
    );
    const focusChat = elementFor(selection.focusNode)?.closest(
        ".chat-messages",
    );
    return anchorChat && anchorChat === focusChat ? selection.toString() : "";
};

export const isNearChatBottom = (
    element: Pick<HTMLElement, "scrollHeight" | "scrollTop" | "clientHeight">,
) => element.scrollHeight - element.scrollTop - element.clientHeight <= 48;

export const ChatSetupCard = (props: {
    setup: NonNullable<GuiAiChat["setup"]>;
    onKey?: (key: string) => void;
}) => {
    const maskedParts = createMemo(() => {
        const value = props.setup.maskedInput ?? "";
        const cursor = Math.max(
            0,
            Math.min(props.setup.inputCursor ?? 0, value.length),
        );
        return [value.slice(0, cursor), value.slice(cursor)] as const;
    });
    return (
        <section class="chat-setup-card" aria-label={props.setup.title}>
            <header>
                <b>{props.setup.title}</b>
                <span>
                    {props.setup.kind === "exaKey" ? "optional" : "required"}
                </span>
            </header>
            <p>{props.setup.detail}</p>
            <Show when={props.setup.maskedInput !== undefined}>
                <pre aria-label="Exa API key input">
                    <span>{maskedParts()[0]}</span>
                    <i class="chat-caret" aria-hidden="true" />
                    <span>
                        {maskedParts()[1] ||
                            (!props.setup.maskedInput ? "Paste API key…" : "")}
                    </span>
                </pre>
            </Show>
            <Show when={props.setup.error}>
                <small role="alert">{props.setup.error}</small>
            </Show>
            <footer>
                <For each={props.setup.actions}>
                    {(action) => (
                        <button
                            type="button"
                            onClick={() => props.onKey?.(action.key)}
                        >
                            {action.label}
                        </button>
                    )}
                </For>
            </footer>
        </section>
    );
};

type GuiChatMessage = GuiAiChat["messages"][number];

type ChatActivityEntry = GuiChatMessage & { live?: boolean };
export type ChatTranscriptItem =
    | { kind: "message"; id: string; message: GuiChatMessage }
    | {
          kind: "activity";
          id: string;
          entries: ChatActivityEntry[];
          live: boolean;
      };

const isActivityMessage = (message: GuiChatMessage) =>
    message.role === "thinking" ||
    message.role === "tool" ||
    (message.role === "assistant" &&
        message.tools.length > 0 &&
        !message.content.trim());

export const chatTranscriptItems = (
    messages: GuiChatMessage[],
    streamingThinking?: string,
    thinkingLive = false,
    workLive = thinkingLive,
): ChatTranscriptItem[] => {
    const items: ChatTranscriptItem[] = [];
    let active: Extract<ChatTranscriptItem, { kind: "activity" }> | undefined;
    for (const message of messages) {
        if (!isActivityMessage(message)) {
            items.push({ kind: "message", id: message.id, message });
            const toolBearingCommentary =
                message.role === "assistant" && message.tools.length > 0;
            if (!toolBearingCommentary) active = undefined;
            continue;
        }
        if (active) {
            active.entries.push(message);
        } else {
            active = {
                kind: "activity",
                id: `activity:${message.id}`,
                entries: [message],
                live: false,
            };
            items.push(active);
        }
    }
    if (thinkingLive) {
        const liveThinking: ChatActivityEntry = {
            id: "streaming-thinking",
            index: messages.length,
            selected: false,
            role: "thinking",
            content: streamingThinking || "Thinking…",
            tools: [],
            live: true,
        };
        if (active) {
            active.entries.push(liveThinking);
            active.live = true;
        } else {
            active = {
                kind: "activity",
                id: "activity:streaming-thinking",
                entries: [liveThinking],
                live: true,
            };
            items.push(active);
        }
    } else if (workLive && active) {
        active.live = true;
    }
    return items;
};

const sameStrings = (left: string[], right: string[]) =>
    left.length === right.length &&
    left.every((value, index) => value === right[index]);

const sameChatMessage = (left: GuiChatMessage, right: GuiChatMessage) =>
    left.id === right.id &&
    left.index === right.index &&
    left.selected === right.selected &&
    left.role === right.role &&
    left.content === right.content &&
    left.model === right.model &&
    left.toolName === right.toolName &&
    sameStrings(left.tools, right.tools) &&
    (left as ChatActivityEntry).live === (right as ChatActivityEntry).live;

export const retainTranscriptItems = (
    previous: ChatTranscriptItem[],
    next: ChatTranscriptItem[],
) => {
    const priorById = new Map(previous.map((item) => [item.id, item]));
    return next.map((item) => {
        const prior = priorById.get(item.id);
        if (!prior || prior.kind !== item.kind) return item;
        if (
            item.kind === "message" &&
            prior.kind === "message" &&
            sameChatMessage(prior.message, item.message)
        )
            return prior;
        if (
            item.kind === "activity" &&
            prior.kind === "activity" &&
            prior.live === item.live &&
            prior.entries.length === item.entries.length &&
            prior.entries.every((entry, index) =>
                sameChatMessage(entry, item.entries[index]),
            )
        )
            return prior;
        return item;
    });
};

const lastActivityEntry = (
    entries: ChatActivityEntry[],
    predicate: (entry: ChatActivityEntry) => boolean,
) => {
    for (let index = entries.length - 1; index >= 0; index -= 1) {
        if (predicate(entries[index])) return entries[index];
    }
    return undefined;
};

export const activitySummary = (entries: ChatActivityEntry[]) => {
    const latestThinking = lastActivityEntry(
        entries,
        (entry) => entry.role === "thinking" && Boolean(entry.content.trim()),
    );
    if (latestThinking) {
        const summary =
            latestThinking.content
                .split("\n")
                .map((line) => line.trim())
                .filter(Boolean)
                .at(-1) || "Thinking…";
        return (
            summary
                .replace(/^#{1,6}\s+/, "")
                .replace(/!\[([^\]]*)\]\([^)]*\)/g, "$1")
                .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
                .replace(/[*_~`]+/g, "")
                .trim() || "Thinking…"
        );
    }
    const latestToolCall = lastActivityEntry(
        entries,
        (entry) => entry.tools.length > 0,
    );
    if (latestToolCall) return `Calling ${latestToolCall.tools.join(", ")}`;
    const latestTool = lastActivityEntry(
        entries,
        (entry) => entry.role === "tool",
    );
    return latestTool?.toolName
        ? `Completed ${latestTool.toolName}`
        : "Agent activity";
};

export const toolResultSummary = (content: string) => {
    const failed = /^\s*(error|failed|failure|denied|cancelled)\b/i.test(
        content.slice(0, 240),
    );
    return `${failed ? "Failed" : "Completed"} · ${content.length.toLocaleString()} characters`;
};

export const ToolCallList = (props: { tools: string[] }) => (
    <Show when={props.tools.length}>
        <details class="tool-call-list">
            <summary>
                <Icon name="chevron-right" size={16} />
                {props.tools.length} tool{" "}
                {props.tools.length === 1 ? "call" : "calls"}
            </summary>
            <div class="tool-chips">
                <For each={props.tools}>{(tool) => <span>{tool}</span>}</For>
            </div>
        </details>
    </Show>
);

export const ChatMessageView = (props: {
    message: GuiChatMessage;
    onSelect?: (index: number) => void;
}) => {
    const [expanded, setExpanded] = createSignal(false);
    let disclosure: HTMLDetailsElement | undefined;
    let identity = "";
    createEffect(() => {
        const next = `${props.message.role}:${props.message.toolName ?? ""}:${props.message.content}`;
        if (identity && identity !== next) {
            setExpanded(false);
            if (disclosure) disclosure.open = false;
        }
        identity = next;
    });
    return (
        <article
            class={`chat-message ${props.message.role}`}
            classList={{ selected: props.message.selected }}
            data-selected={props.message.selected || undefined}
            aria-current={props.message.selected ? "true" : undefined}
            onClick={(event) => {
                if ((event.target as Element).closest("a, button, summary"))
                    return;
                props.onSelect?.(props.message.index);
            }}
        >
            <Show
                when={props.message.role === "tool"}
                fallback={
                    <>
                        <header>
                            <b>{props.message.role}</b>
                            <small>{props.message.model}</small>
                        </header>
                        <Markdown text={props.message.content} />
                        <ToolCallList tools={props.message.tools} />
                    </>
                }
            >
                <details
                    ref={disclosure}
                    class="tool-result"
                    onToggle={(event) => setExpanded(event.currentTarget.open)}
                >
                    <summary>
                        <Icon name="chevron-right" size={16} />
                        <span>
                            <b>{props.message.toolName || "Tool result"}</b>
                            <small>
                                {toolResultSummary(props.message.content)}
                            </small>
                        </span>
                        <em>Details</em>
                    </summary>
                    <Show when={expanded()}>
                        <Markdown text={props.message.content} />
                    </Show>
                </details>
            </Show>
        </article>
    );
};

export const ChatPanel = (props: {
    chat: GuiAiChat;
    revision?: number;
    focusInput: () => void;
    bindInput?: (input: HTMLTextAreaElement | undefined) => void;
    onInputUpdate?: (update: ChatInputUpdate) => Promise<void>;
    onSetupKey?: (key: string) => void;
    onInputWidth?: (columns: number) => void;
    onRemoveImage?: (index: number) => void;
    onProfile?: (profile: string) => void;
    onReasoningEffort?: (effort: string) => void;
    onYolo?: () => void;
    onComprehension?: () => void;
    onMessage?: (index: number) => void;
    onAgent?: (agentId?: string) => void;
    onQueuedAction?: (
        id: number,
        action: "select" | "recall" | "remove",
    ) => void;
}) => {
    const [following, setFollowing] = createSignal(true);
    let transcript!: HTMLDivElement;
    let messageCount = props.chat.messages.length;
    let selectedMessageId: string | undefined;
    const transcriptItems = createMemo<ChatTranscriptItem[]>(
        (previous = []) =>
            retainTranscriptItems(
                previous,
                chatTranscriptItems(
                    props.chat.messages,
                    props.chat.streamingThinking,
                    props.chat.thinkingLive,
                    props.chat.activity !== "idle",
                ),
            ),
        [],
    );
    const transcriptEmpty = () =>
        transcriptItems().length === 0 &&
        !props.chat.streaming &&
        props.chat.queuedInputs.length === 0;

    createEffect(() => {
        const selected = props.chat.messages.find(
            (message) => message.selected,
        )?.id;
        if (selected === selectedMessageId) return;
        selectedMessageId = selected;
        if (!selected) return;
        setFollowing(false);
        queueMicrotask(() =>
            transcript
                .querySelector<HTMLElement>("[data-selected='true']")
                ?.scrollIntoView?.({ block: "nearest" }),
        );
    });

    const jumpToLatest = () => {
        transcript.scrollTop = transcript.scrollHeight;
        setFollowing(true);
    };

    createEffect(() => {
        const messages = props.chat.messages;
        const latest = messages.at(-1);
        const queued = props.chat.queuedInputs;
        const revision = [
            messages.length,
            latest?.content.length ?? 0,
            props.chat.streaming?.length ?? 0,
            props.chat.streamingThinking?.length ?? 0,
            queued.length,
            queued.at(-1)?.content.length ?? 0,
            props.chat.approval?.length ?? 0,
        ];
        if (messages.length > messageCount && latest?.role === "user")
            setFollowing(true);
        messageCount = messages.length;
        void revision;
        queueMicrotask(() => {
            if (following()) jumpToLatest();
        });
    });

    return (
        <section
            class="side-panel ai-panel"
            aria-label="AI chat"
            aria-busy={props.chat.activity !== "idle"}
        >
            <header class="side-panel-header">
                <div>
                    <b>AI chat</b>
                    <ChatModelPicker
                        profile={props.chat.profile}
                        profiles={props.chat.profiles}
                        reasoningEffort={props.chat.reasoningEffort}
                        reasoningEffortSelection={
                            props.chat.reasoningEffortSelection
                        }
                        reasoningEfforts={props.chat.reasoningEfforts}
                        onProfile={props.onProfile}
                        onReasoningEffort={props.onReasoningEffort}
                        focusInput={props.focusInput}
                    />
                </div>
                <div class="chat-policy-controls">
                    <button
                        type="button"
                        classList={{ enabled: props.chat.yoloMode }}
                        aria-pressed={props.chat.yoloMode}
                        title={
                            props.chat.yoloMode
                                ? "Disable approval bypass for this chat"
                                : "Bypass Terra and interactive approvals for this chat"
                        }
                        onClick={() => {
                            props.onYolo?.();
                            queueMicrotask(props.focusInput);
                        }}
                    >
                        YOLO {props.chat.yoloMode ? "ON" : "OFF"}
                    </button>
                    <button
                        type="button"
                        classList={{
                            enabled: props.chat.comprehensionPolicy !== "off",
                        }}
                        aria-pressed={props.chat.comprehensionPolicy !== "off"}
                        title={
                            props.chat.comprehensionCheckpoint
                                ? `Checkpoint: ${props.chat.comprehensionCheckpoint}`
                                : "Require demonstrated comprehension at the configured boundary"
                        }
                        onClick={() => {
                            props.onComprehension?.();
                            queueMicrotask(props.focusInput);
                        }}
                    >
                        COMPREHENSION
                        {props.chat.comprehensionPolicy === "off"
                            ? " OFF"
                            : `: ${props.chat.comprehensionPolicy.toUpperCase()}`}
                    </button>
                </div>
            </header>
            <Show when={props.chat.agents.length}>
                <section class="chat-agents" aria-label="Agent navigation">
                    <button
                        type="button"
                        aria-current={
                            !props.chat.selectedAgentId ? "true" : undefined
                        }
                        classList={{
                            selected: !props.chat.selectedAgentId,
                            cursor:
                                props.chat.focus === "treePanel" &&
                                props.chat.agentCursor === 0,
                        }}
                        onClick={() => {
                            props.onAgent?.();
                            queueMicrotask(props.focusInput);
                        }}
                    >
                        <span>
                            <b>Primary conversation</b>
                            <small>{props.chat.profile}</small>
                        </span>
                        <em>root</em>
                    </button>
                    <For each={props.chat.agents}>
                        {(agent, index) => (
                            <button
                                type="button"
                                aria-current={
                                    props.chat.selectedAgentId === agent.id
                                        ? "true"
                                        : undefined
                                }
                                classList={{
                                    selected:
                                        props.chat.selectedAgentId === agent.id,
                                    followed:
                                        props.chat.followedAgentId === agent.id,
                                    cursor:
                                        props.chat.focus === "treePanel" &&
                                        props.chat.agentCursor === index() + 1,
                                }}
                                style={{
                                    "padding-left": `${9 + agent.depth * 12}px`,
                                }}
                                onClick={() => {
                                    props.onAgent?.(agent.id);
                                    queueMicrotask(props.focusInput);
                                }}
                            >
                                <Show
                                    when={
                                        props.chat.followedAgentId === agent.id
                                    }
                                >
                                    <Icon name="status-success" size={16} />
                                </Show>
                                <span>
                                    <b>{agent.taskName}</b>
                                    <small>{agent.model}</small>
                                </span>
                                <em>
                                    {props.chat.followedAgentId === agent.id
                                        ? "following · "
                                        : ""}
                                    {agent.lifecycle.replaceAll("_", " ")}
                                </em>
                            </button>
                        )}
                    </For>
                </section>
            </Show>
            <div class="chat-transcript">
                <div
                    class="chat-messages"
                    ref={transcript}
                    onScroll={() => setFollowing(isNearChatBottom(transcript))}
                >
                    <Show when={transcriptEmpty()}>
                        <div class="panel-empty chat-empty">
                            <Icon name="ai-spark" size={20} tone="accent" />
                            <b>Start a conversation</b>
                            <span>
                                Ask about the current file, selection, or
                                workspace.
                            </span>
                        </div>
                    </Show>
                    <Index each={transcriptItems()}>
                        {(item) => (
                            <Show
                                when={
                                    item().kind === "activity"
                                        ? (item() as Extract<
                                              ChatTranscriptItem,
                                              { kind: "activity" }
                                          >)
                                        : undefined
                                }
                                fallback={
                                    <ChatMessageView
                                        message={
                                            (
                                                item() as Extract<
                                                    ChatTranscriptItem,
                                                    { kind: "message" }
                                                >
                                            ).message
                                        }
                                        onSelect={props.onMessage}
                                    />
                                }
                            >
                                {(activity) => (
                                    <ChatActivityGroup
                                        item={activity()}
                                        onSelect={props.onMessage}
                                    />
                                )}
                            </Show>
                        )}
                    </Index>
                    <Show when={props.chat.streaming}>
                        {(content) => (
                            <article class="chat-message assistant streaming">
                                <header>
                                    <b>assistant</b>
                                    <small>streaming</small>
                                </header>
                                <Markdown text={content()} />
                            </article>
                        )}
                    </Show>
                    <For each={props.chat.queuedInputs}>
                        {(item) => (
                            <QueuedChatMessage
                                item={item}
                                onAction={(id, action) => {
                                    props.onQueuedAction?.(id, action);
                                    queueMicrotask(props.focusInput);
                                }}
                            />
                        )}
                    </For>
                </div>
                <Show when={!following()}>
                    <button
                        type="button"
                        class="chat-jump"
                        onClick={() => {
                            jumpToLatest();
                            props.focusInput();
                        }}
                    >
                        {props.chat.activity !== "idle"
                            ? "New activity"
                            : "New messages"}
                        <Icon name="chevron-down" size={16} />
                    </button>
                </Show>
            </div>
            <Show when={props.chat.approval}>
                {(approval) => (
                    <div class="approval-card" role="status" aria-live="polite">
                        <b>Approval required</b>
                        <span>{approval()}</span>
                        <small>Use the keyboard choices shown by Ovim.</small>
                    </div>
                )}
            </Show>
            <Show when={props.chat.setup}>
                {(setup) => (
                    <ChatSetupCard setup={setup()} onKey={props.onSetupKey} />
                )}
            </Show>
            <ChatComposer
                chat={props.chat}
                revision={props.revision}
                bindInput={props.bindInput}
                onUpdate={props.onInputUpdate}
                onWidth={props.onInputWidth}
                onRemoveImage={props.onRemoveImage}
            />
        </section>
    );
};

function App() {
    const native = isTauri();
    const compactDockQuery = window.matchMedia?.("(max-width: 1439px)");
    const [view, setView] = createSignal<GuiSnapshot>(mockSnapshot);
    const [error, setError] = createSignal("");
    const [connected, setConnected] = createSignal(!native);
    const [composition, setComposition] = createSignal("");
    const [pendingExit, setPendingExit] = createSignal<
        "close" | "quit" | undefined
    >();
    const [workbenchView, setWorkbenchView] = createSignal<
        "source" | "vector" | "browser"
    >("source");
    const [browserState, setBrowserState] = createSignal<BrowserState>({
        sessions: [],
        maxSessions: 8,
    });
    const [activeBrowserId, setActiveBrowserId] = createSignal<string>();
    const [browserOpening, setBrowserOpening] = createSignal(false);
    const [vectorPreview, setVectorPreview] = createSignal<VectorPreview>();
    const [vectorPreviewError, setVectorPreviewError] = createSignal("");
    const [vectorPreviewLoading, setVectorPreviewLoading] = createSignal(false);
    const [vectorRefresh, setVectorRefresh] = createSignal(0);
    const [vectorFeedback, setVectorFeedback] = createSignal("");
    const [vectorFeedbackStatus, setVectorFeedbackStatus] = createSignal("");
    const [compactDocks, setCompactDocks] = createSignal(
        compactDockQuery?.matches ?? false,
    );
    const [activeDock, setActiveDock] = createSignal<"explorer" | "context">(
        mockSnapshot.aiChat || mockSnapshot.testPanel || mockSnapshot.debug
            ? "context"
            : "explorer",
    );
    const [activeContextPanel, setActiveContextPanel] = createSignal<
        "ai" | "tests" | "debug" | "diff"
    >("ai");
    const [diffDockOpen, setDiffDockOpen] = createSignal(false);
    const [gdiff, setGdiff] = createSignal<GuiGdiffReview>();
    let editorBody!: HTMLDivElement;
    let inputSink!: HTMLTextAreaElement;
    let chatInput: HTMLTextAreaElement | undefined;
    let lspDialog: HTMLElement | undefined;
    let cellWidth = FALLBACK_CELL_WIDTH;
    let composing = false;
    let ignoreNextInput = false;
    let wheelRemainder = 0;
    let lastDimensions = { columns: 0, rows: 0 };
    let latestSnapshotRevision: number | undefined;
    const walkthrough = createMemo(() => view().aiChat?.codeExplanation);
    const hasContextDock = createMemo(() =>
        Boolean(
            !walkthrough() &&
            (view().aiChat ||
                view().testPanel ||
                view().debug ||
                diffDockOpen()),
        ),
    );
    let hadContextDock = hasContextDock();
    let previousContextAvailability = {
        ai: Boolean(view().aiChat),
        tests: Boolean(view().testPanel),
        debug: Boolean(view().debug),
        diff: diffDockOpen(),
    };
    let layoutWorkspace = "";
    let vectorFilePath: string | undefined;
    const activeStrokPath = createMemo(() => {
        const filePath = view().filePath;
        return filePath?.toLowerCase().endsWith(".strok")
            ? filePath
            : undefined;
    });
    const activeBrowser = createMemo<BrowserSession | undefined>(() => {
        const sessionId = activeBrowserId();
        return browserState().sessions.find(
            (session) => session.sessionId === sessionId,
        );
    });
    const activeStrok = createMemo(() => Boolean(activeStrokPath()));
    const activeBufferRevision = createMemo(() => view().bufferRevision);

    createEffect(() => {
        const filePath = view().filePath;
        if (filePath !== vectorFilePath) {
            vectorFilePath = filePath;
            setVectorPreview(undefined);
            setVectorPreviewError("");
            setVectorPreviewLoading(false);
            setVectorFeedback("");
            setVectorFeedbackStatus("");
        }
        if (!filePath?.toLowerCase().endsWith(".strok")) {
            if (workbenchView() === "vector") setWorkbenchView("source");
        }
    });

    createEffect(() => {
        if (!native || !activeStrok() || workbenchView() !== "vector") return;
        void activeStrokPath();
        void activeBufferRevision();
        void vectorRefresh();
        let cancelled = false;
        const timer = window.setTimeout(() => {
            setVectorPreviewLoading(true);
            setVectorPreviewError("");
            void invoke<VectorPreview>("gui_vector_preview")
                .then((preview) => {
                    if (!cancelled) setVectorPreview(preview);
                })
                .catch((reason) => {
                    if (!cancelled) setVectorPreviewError(String(reason));
                })
                .finally(() => {
                    if (!cancelled) setVectorPreviewLoading(false);
                });
        }, 180);
        onCleanup(() => {
            cancelled = true;
            window.clearTimeout(timer);
            setVectorPreviewLoading(false);
        });
    });

    const diffWorkspace = () => view().workspacePath || "";

    const layoutStorage = () => {
        try {
            return window.localStorage;
        } catch {
            return undefined;
        }
    };

    createEffect(() => {
        const workspace = workspaceLayoutIdentity(view());
        if (workspace === layoutWorkspace) return;
        layoutWorkspace = workspace;
        const preference = readWorkbenchLayout(layoutStorage(), workspace);
        if (!preference) return;
        setActiveDock(preference.activeDock);
        setActiveContextPanel(preference.activeContextPanel);
        if (preference.activeContextPanel === "diff") setDiffDockOpen(true);
    });

    createEffect(() => {
        const preference = {
            activeDock: activeDock(),
            activeContextPanel: activeContextPanel(),
        };
        if (!layoutWorkspace) return;
        writeWorkbenchLayout(layoutStorage(), layoutWorkspace, preference);
    });

    createEffect(() => {
        const hasContext = hasContextDock();
        const hasExplorer = Boolean(view().fileTree);
        if (hasContext && !hadContextDock) setActiveDock("context");
        else if (!hasContext && activeDock() === "context")
            setActiveDock("explorer");
        else if (hasContext && !hasExplorer && activeDock() === "explorer")
            setActiveDock("context");
        hadContextDock = hasContext;
    });

    createEffect(() => {
        const next = {
            ai: Boolean(view().aiChat),
            tests: Boolean(view().testPanel),
            debug: Boolean(view().debug),
            diff: diffDockOpen(),
        };
        if (next.ai && !previousContextAvailability.ai)
            setActiveContextPanel("ai");
        if (next.tests && !previousContextAvailability.tests)
            setActiveContextPanel("tests");
        if (next.debug && !previousContextAvailability.debug)
            setActiveContextPanel("debug");
        if (next.diff && !previousContextAvailability.diff)
            setActiveContextPanel("diff");
        previousContextAvailability = next;
    });

    const dimensions = () => {
        const paneTree = editorBody?.querySelector<HTMLElement>(".pane-tree");
        const paneColumns = Math.floor(
            (paneTree?.clientWidth || editorBody?.clientWidth || 960) /
                cellWidth,
        );
        // The shared core viewport contract consumes full terminal dimensions and
        // subtracts its own tree/status/tab chrome. Add those cells back after
        // measuring the DOM's already-narrowed editor surface.
        const coreChrome = view().fileTree ? 50 : 0;
        return {
            columns: Math.max(20, paneColumns + coreChrome),
            rows: Math.max(
                5,
                Math.floor((editorBody?.clientHeight || 600) / LINE_HEIGHT) +
                    2 +
                    (view().tabs.length > 1 ? 1 : 0),
            ),
        };
    };

    const syncDimensions = () => {
        if (!native) return;
        const next = dimensions();
        if (
            next.columns === lastDimensions.columns &&
            next.rows === lastDimensions.rows
        )
            return;
        lastDimensions = next;
        void invoke("gui_snapshot", next).catch((reason) =>
            setError(String(reason)),
        );
    };

    const accept = (snapshot: GuiSnapshot) => {
        if (!shouldAcceptRevision(latestSnapshotRevision, snapshot.revision))
            return;
        latestSnapshotRevision = snapshot.revision;
        const chatOpened = !view().aiChat && Boolean(snapshot.aiChat);
        const chatClosed = Boolean(view().aiChat) && !snapshot.aiChat;
        const coreDialogClosed =
            Boolean(view().picker || view().lspManager) &&
            !snapshot.picker &&
            !snapshot.lspManager;
        setView((previous) => retainProjection(previous, snapshot));
        setConnected(true);
        setError("");
        requestAnimationFrame(syncDimensions);
        if (chatOpened) queueMicrotask(focusChatInput);
        if (chatClosed) queueMicrotask(focusEditorInput);
        if (coreDialogClosed) queueMicrotask(focusEditorInput);
        if (snapshot.shouldQuit && native) void windowAction("close-approved");
    };

    const mutateStrict = async (
        command: string,
        args: Record<string, unknown>,
    ) => {
        if (!native) return;
        try {
            await invoke(command, args);
        } catch (reason) {
            setError(String(reason));
            throw reason;
        }
    };

    const mutate = async (command: string, args: Record<string, unknown>) => {
        try {
            await mutateStrict(command, args);
        } catch {
            // `mutateStrict` has already projected the failure into the GUI status.
        }
    };

    const focusEditorInput = () => inputSink?.focus({ preventScroll: true });
    const focusChatInput = () => {
        if (chatInput?.isConnected) chatInput.focus({ preventScroll: true });
        else focusEditorInput();
    };
    const acceptBrowserState = (next: BrowserState) => {
        setBrowserState(next);
        if (workbenchView() !== "browser") return;
        const selected = activeBrowserId();
        if (
            !selected ||
            !next.activeSessionId ||
            !next.sessions.some((session) => session.sessionId === selected)
        ) {
            setActiveBrowserId(undefined);
            setWorkbenchView("source");
            return;
        }
        if (next.activeSessionId !== selected)
            setActiveBrowserId(next.activeSessionId);
    };
    const focusBrowser = () => {
        const sessionId = activeBrowserId();
        if (native && sessionId)
            void invoke("gui_browser_toolbar", {
                sessionId,
                action: "focus",
            }).catch(() => {});
    };
    const openBrowserSession = async () => {
        const current = browserState();
        if (
            !native ||
            browserOpening() ||
            current.sessions.length >= current.maxSessions
        )
            return;
        setBrowserOpening(true);
        setError("");
        try {
            const next = await invoke<BrowserState>("gui_browser_open");
            acceptBrowserState(next);
            const sessionId = next.activeSessionId;
            if (sessionId) {
                setActiveBrowserId(sessionId);
                setWorkbenchView("browser");
                queueMicrotask(focusBrowser);
            }
        } catch (reason) {
            setError(String(reason));
        } finally {
            setBrowserOpening(false);
        }
    };
    const activateBrowserSession = (sessionId: string) => {
        if (
            !browserState().sessions.some(
                (session) => session.sessionId === sessionId,
            )
        )
            return;
        if (!native) {
            setActiveBrowserId(sessionId);
            setWorkbenchView("browser");
            return;
        }
        void invoke<BrowserState>("gui_browser_activate", { sessionId })
            .then((next) => {
                acceptBrowserState(next);
                setActiveBrowserId(sessionId);
                setWorkbenchView("browser");
                queueMicrotask(focusBrowser);
            })
            .catch((reason) => {
                setError(String(reason));
            });
    };
    const focusPrimaryInput = () => {
        if (workbenchView() === "browser") {
            focusBrowser();
            return;
        }
        if (
            activeDock() === "context" &&
            activeContextPanel() === "ai" &&
            view().aiChat
        )
            focusChatInput();
        else focusEditorInput();
    };

    const sendKey = (input: GuiKeyInput) => mutate("gui_key", { input });
    const sendLiteral = async (keys: string) => {
        for (const key of keys) {
            await sendKey({
                key,
                shift: key.toUpperCase() === key && key.toLowerCase() !== key,
                control: false,
                alt: false,
                meta: false,
            });
        }
    };
    const windowAction = async (action: string) => {
        if (!native) return;
        try {
            await invoke<void>("gui_window_action", { action });
        } catch (reason) {
            setError(String(reason));
        }
    };

    const editorCommand = (command: string) =>
        mutateStrict("gui_editor_command", { command });

    const requestExit = (kind: "close" | "quit") => {
        if (kind === "quit" || view().hasUnsavedChanges) {
            setPendingExit(kind);
            return;
        }
        void editorCommand("qa");
    };

    const saveAndExit = async () => {
        try {
            await editorCommand("wa");
            setPendingExit();
            await editorCommand("qa");
        } catch {
            // Keep the confirmation open; the status line explains the save failure.
        }
    };

    const discardAndExit = () => {
        setPendingExit();
        void editorCommand("qa!");
    };

    const selectAllEditorText = async () => {
        focusEditorInput();
        await sendKey({
            key: "Escape",
            shift: false,
            control: false,
            alt: false,
            meta: false,
        });
        await sendLiteral("ggVG");
    };

    const performMenuAction = (action: string) => {
        const active = document.activeElement;
        const nativeEditor =
            active instanceof HTMLTextAreaElement && active !== inputSink;
        switch (action) {
            case "file.save":
                void editorCommand("w");
                break;
            case "file.save-all":
                void editorCommand("wa");
                break;
            case "file.close":
                requestExit("close");
                break;
            case "app.quit":
                requestExit("quit");
                break;
            case "edit.undo":
                if (nativeEditor) document.execCommand("undo");
                else void editorCommand("undo");
                break;
            case "edit.redo":
                if (nativeEditor) document.execCommand("redo");
                else void editorCommand("redo");
                break;
            case "edit.select-all":
                if (
                    active instanceof HTMLInputElement ||
                    active instanceof HTMLTextAreaElement
                )
                    active.select();
                else void selectAllEditorText();
                break;
            case "edit.find":
                focusEditorInput();
                void sendKey({
                    key: "/",
                    shift: false,
                    control: false,
                    alt: false,
                    meta: false,
                });
                break;
        }
    };

    const toggleExplorer = () => {
        if (
            compactDocks() &&
            view().fileTree &&
            hasContextDock() &&
            activeDock() === "context"
        ) {
            setActiveDock("explorer");
            queueMicrotask(focusEditorInput);
            return;
        }
        setActiveDock("explorer");
        focusEditorInput();
        void sendLiteral("-");
    };

    const toggleAiChat = () => {
        setActiveContextPanel("ai");
        if (compactDocks() && view().aiChat && activeDock() === "explorer") {
            setActiveDock("context");
            queueMicrotask(focusChatInput);
            return;
        }
        setActiveDock("context");
        focusChatInput();
        void sendLiteral("  ");
    };

    const toggleDiff = () => {
        const wasActive =
            diffDockOpen() &&
            activeDock() === "context" &&
            activeContextPanel() === "diff";
        setDiffDockOpen(true);
        setActiveContextPanel("diff");
        if (compactDocks() && wasActive) {
            setActiveDock("explorer");
            queueMicrotask(focusEditorInput);
            return;
        }
        setActiveDock("context");
    };

    const runEditorShortcut = async (keys: string) => {
        focusEditorInput();
        await sendLiteral(keys);
    };

    const addVectorFeedbackToChat = async () => {
        const feedback = vectorFeedback().trim();
        if (!feedback) return;
        try {
            await mutateStrict("gui_vector_feedback", { feedback });
            setVectorFeedback("");
            setVectorFeedbackStatus(
                "Added to the AI chat draft — review and send when ready.",
            );
            setActiveContextPanel("ai");
            setActiveDock("context");
            queueMicrotask(focusChatInput);
        } catch {
            setVectorFeedbackStatus("");
        }
    };

    const selectWorkbenchTab = (position: number) => {
        const tabs = view().tabs;
        if (position === tabs.length && activeStrok()) {
            setWorkbenchView("vector");
            return;
        }
        const browserPosition = tabs.length + (activeStrok() ? 1 : 0);
        const browser = browserState().sessions[position - browserPosition];
        if (browser) {
            activateBrowserSession(browser.sessionId);
            return;
        }
        const tab = tabs[position];
        if (!tab) return;
        setWorkbenchView("source");
        void mutate("gui_select_tab", { index: tab.index });
    };

    const handleTabNavigation = (event: KeyboardEvent, position: number) => {
        const tabs = view().tabs;
        const tabCount =
            tabs.length +
            (activeStrok() ? 1 : 0) +
            browserState().sessions.length;
        if (tabCount < 2) return;
        let next = position;
        if (event.key === "ArrowRight") next = (position + 1) % tabCount;
        else if (event.key === "ArrowLeft")
            next = (position - 1 + tabCount) % tabCount;
        else if (event.key === "Home") next = 0;
        else if (event.key === "End") next = tabCount - 1;
        else return;
        event.preventDefault();
        selectWorkbenchTab(next);
        queueMicrotask(() =>
            document
                .querySelector<HTMLElement>(
                    `[data-workbench-tab-index="${next}"]`,
                )
                ?.focus({ preventScroll: true }),
        );
    };

    const themeVars = createMemo(() => ({
        ...themeVariables(view().theme),
        "--cell-width": `${cellWidth}px`,
    }));

    const breadcrumbs = createMemo(() => {
        const path = view().filePath;
        if (!path) return [view().fileName];
        return path.split(/[\\/]/).filter(Boolean).slice(-4);
    });

    const handleKeyDown = (event: KeyboardEvent) => {
        if (
            event.isComposing ||
            event.key === "Process" ||
            event.key === "Dead"
        )
            return;
        const target = event.target as Element | null;
        const primaryModifier = /Mac|iPhone|iPad/.test(navigator.platform)
            ? event.metaKey
            : event.ctrlKey;
        if (primaryModifier && event.key.toLowerCase() === "s") {
            event.preventDefault();
            performMenuAction(event.altKey ? "file.save-all" : "file.save");
            return;
        }
        if (primaryModifier && event.key.toLowerCase() === "w") {
            event.preventDefault();
            requestExit("close");
            return;
        }
        if (primaryModifier && event.key.toLowerCase() === "q") {
            event.preventDefault();
            requestExit("quit");
            return;
        }
        const nativeControl = isGuiNativeControl(target, inputSink);
        if (primaryModifier && !nativeControl) {
            const key = event.key.toLowerCase();
            if (key === "z" || key === "a" || key === "f") {
                event.preventDefault();
                performMenuAction(
                    key === "z"
                        ? event.shiftKey
                            ? "edit.redo"
                            : "edit.undo"
                        : key === "a"
                          ? "edit.select-all"
                          : "edit.find",
                );
                return;
            }
        }
        if (event.key === "Tab" && target?.closest?.("[data-gui-core-dialog]"))
            return;
        if (nativeControl) return;
        const clipboardModifier = /Mac|iPhone|iPad/.test(navigator.platform)
            ? event.metaKey
            : event.ctrlKey && event.shiftKey;
        if (
            clipboardModifier &&
            ["c", "v", "x"].includes(event.key.toLowerCase())
        )
            return;
        const input = guiKeyInput(event);
        if (!input) return;
        event.preventDefault();
        void sendKey(input);
    };

    const handlePaste = (event: ClipboardEvent) => {
        const target = event.target as Element | null;
        const nativeTextOwner =
            target !== inputSink &&
            Boolean(
                target?.closest?.("input, textarea, [contenteditable='true']"),
            );
        const image =
            Array.from(event.clipboardData?.items ?? [])
                .find((item) => imageExtension(item.type))
                ?.getAsFile() ??
            Array.from(event.clipboardData?.files ?? []).find((file) =>
                imageExtension(file.type),
            );
        if (image) {
            if (nativeTextOwner && target !== chatInput) return;
            event.preventDefault();
            if (image.size > MAX_IMAGE_BYTES) {
                setError("Clipboard image exceeds the 20 MiB limit");
                return;
            }
            void image
                .arrayBuffer()
                .then((data) =>
                    invoke("gui_attach_image", new Uint8Array(data), {
                        headers: {
                            "x-ovim-image-extension": imageExtension(
                                image.type,
                            ),
                        },
                    }),
                )
                .catch((reason) => setError(String(reason)));
            return;
        }
        if (nativeTextOwner) return;
        const text = event.clipboardData?.getData("text/plain");
        if (!text) return;
        event.preventDefault();
        void mutate("gui_paste", { text });
    };

    const handleCopy = (event: ClipboardEvent) => {
        const chatText = chatSelectionText();
        if (chatText) {
            event.clipboardData?.setData("text/plain", chatText);
            event.preventDefault();
            return;
        }
        const target = event.target as Element | null;
        if (
            target !== inputSink &&
            target?.closest?.("input, textarea, [contenteditable='true']")
        )
            return;
        const text = view().selectionText;
        if (!text) return;
        event.clipboardData?.setData("text/plain", text);
        event.preventDefault();
    };

    const handleCut = (event: ClipboardEvent) => {
        const chatText = chatSelectionText();
        if (chatText) {
            event.clipboardData?.setData("text/plain", chatText);
            event.preventDefault();
            return;
        }
        const target = event.target as Element | null;
        if (
            target !== inputSink &&
            target?.closest?.("input, textarea, [contenteditable='true']")
        )
            return;
        const text = view().selectionText;
        if (!text) return;
        event.clipboardData?.setData("text/plain", text);
        event.preventDefault();
        void sendKey({
            key: "d",
            shift: false,
            control: false,
            alt: false,
            meta: false,
        });
    };

    const handleCompositionStart = () => {
        composing = true;
        setComposition("");
    };

    const handleCompositionUpdate = (event: CompositionEvent) =>
        setComposition(event.data);

    const handleCompositionEnd = (event: CompositionEvent) => {
        composing = false;
        setComposition("");
        ignoreNextInput = true;
        if (event.data) void mutate("gui_paste", { text: event.data });
        queueMicrotask(() => {
            inputSink.value = "";
        });
    };

    const handleTextInput = (event: InputEvent) => {
        if (composing) return;
        if (ignoreNextInput) {
            ignoreNextInput = false;
            return;
        }
        if (event.inputType.startsWith("insert") && event.data) {
            void mutate("gui_paste", { text: event.data });
        }
        inputSink.value = "";
    };

    const handleWheel = async (event: WheelEvent) => {
        const pane = (event.target as Element | null)?.closest<HTMLElement>(
            ".editor-pane",
        );
        if (!pane) return;
        event.preventDefault();
        const scale =
            event.deltaMode === WheelEvent.DOM_DELTA_LINE
                ? LINE_HEIGHT
                : event.deltaMode === WheelEvent.DOM_DELTA_PAGE
                  ? editorBody.clientHeight
                  : 1;
        wheelRemainder += event.deltaY * scale;
        const count = Math.min(
            8,
            Math.floor(Math.abs(wheelRemainder) / LINE_HEIGHT),
        );
        if (count === 0) return;
        const direction = Math.sign(wheelRemainder);
        wheelRemainder -= direction * count * LINE_HEIGHT;
        const paneIndex = Number(pane.dataset.pane);
        if (Number.isFinite(paneIndex) && !pane.classList.contains("focused")) {
            await mutate("gui_focus_pane", { index: paneIndex });
        }
        const key = direction > 0 ? "e" : "y";
        for (let index = 0; index < count; index += 1) {
            await sendKey({
                key,
                shift: false,
                control: true,
                alt: false,
                meta: false,
            });
        }
    };

    const setCursor = (
        event: MouseEvent,
        pane: number,
        line: number,
        displayStart: number,
    ) => {
        event.stopPropagation();
        focusEditorInput();
        const target = event.currentTarget as HTMLElement;
        // The content element itself is translated by the horizontal scroll
        // offset, so its bounding box already starts at display column zero.
        const displayColumn =
            displayStart +
            Math.max(
                0,
                Math.floor(
                    (event.clientX - target.getBoundingClientRect().left) /
                        cellWidth,
                ),
            );
        void mutate("gui_set_cursor", { pane, line: line - 1, displayColumn });
    };

    const pickerChars = (text: string, matched: number[]) => {
        const selected = new Set(matched);
        return Array.from(text).map((char, index) => ({
            char,
            matched: selected.has(index),
        }));
    };

    const lineIsInWalkthrough = (line: number, focused: boolean) => {
        const page = walkthrough()?.page;
        return Boolean(
            focused &&
            page?.kind === "code" &&
            line >= page.startLine &&
            line <= page.endLine,
        );
    };

    const diagnosticIcon = (
        severity: string,
    ): {
        name: "status-error" | "status-warning" | "status-info";
        tone: IconTone;
    } => {
        if (severity === "error")
            return { name: "status-error", tone: "error" };
        if (severity === "warning")
            return { name: "status-warning", tone: "warning" };
        return { name: "status-info", tone: "information" };
    };

    const inlineOverlayStyle = (
        line: number,
        displayColumn: number,
        width: number,
        height: number,
    ) => {
        const position = anchoredOverlayPosition({
            anchorX:
                Math.max(0, displayColumn - view().horizontalOffset) *
                    cellWidth +
                66,
            anchorY: Math.max(0, line - view().firstLine + 1) * LINE_HEIGHT + 6,
            containerWidth: editorBody?.clientWidth || 960,
            containerHeight: editorBody?.clientHeight || 600,
            preferredWidth: width,
            preferredHeight: height,
        });
        return {
            left: `${position.left}px`,
            top: `${position.top}px`,
            width: `${position.width}px`,
            "max-height": `${position.height}px`,
        };
    };

    const PaneView = (props: { pane: GuiPane }) => (
        <section
            class="editor-pane"
            data-pane={props.pane.index}
            classList={{
                focused: props.pane.focused,
                single: view().panes.length === 1,
                "insert-mode": view().mode === "INSERT",
            }}
            onMouseDown={() => {
                inputSink.focus({ preventScroll: true });
                if (!props.pane.focused)
                    void mutate("gui_focus_pane", { index: props.pane.index });
            }}
        >
            <Show when={view().panes.length > 1}>
                <header class="pane-title">
                    <span>
                        {props.pane.fileName}
                        {props.pane.modified ? " •" : ""}
                    </span>
                    <small>
                        {props.pane.cursor.line + 1}:
                        {props.pane.cursor.column + 1}
                    </small>
                </header>
            </Show>
            <div class="code-viewport">
                <For each={props.pane.lines}>
                    {(line) => (
                        <div
                            class="code-line"
                            classList={{
                                current: line.current && props.pane.focused,
                                walkthrough: lineIsInWalkthrough(
                                    line.number,
                                    props.pane.focused,
                                ),
                            }}
                        >
                            <span class={`change-mark ${line.git || ""}`} />
                            <span
                                class={`diagnostic-mark ${line.diagnostic || ""}`}
                            >
                                <Show when={line.diagnostic}>
                                    {(severity) => {
                                        const status =
                                            diagnosticIcon(severity());
                                        return (
                                            <Icon
                                                name={status.name}
                                                tone={status.tone}
                                                size={16}
                                            />
                                        );
                                    }}
                                </Show>
                            </span>
                            <span class="line-number">
                                {line.continuation ? "" : line.number}
                            </span>
                            <span
                                class="line-content"
                                style={{
                                    transform: `translateX(-${Math.max(0, props.pane.horizontalOffset - line.displayStart) * cellWidth}px)`,
                                }}
                                onMouseDown={(event) =>
                                    setCursor(
                                        event,
                                        props.pane.index,
                                        line.number,
                                        line.displayStart,
                                    )
                                }
                            >
                                <For each={line.segments}>
                                    {(segment) => (
                                        <span
                                            class="code-segment"
                                            classList={{
                                                cursor:
                                                    segment.cursor &&
                                                    props.pane.focused,
                                                selected: segment.selected,
                                                "search-match":
                                                    segment.searchMatch,
                                            }}
                                            style={{
                                                color: segment.token
                                                    ? view().theme.syntax[
                                                          segment.token
                                                      ]
                                                    : undefined,
                                                width: `${segment.cells * cellWidth}px`,
                                            }}
                                        >
                                            {segment.text}
                                        </span>
                                    )}
                                </For>
                            </span>
                        </div>
                    )}
                </For>
            </div>
            <div class="overview-ruler" aria-hidden="true">
                <For each={props.pane.lines}>
                    {(line) => (
                        <Show
                            when={
                                (line.current && props.pane.focused) ||
                                line.diagnostic ||
                                line.git
                            }
                        >
                            <span
                                classList={{
                                    current: line.current && props.pane.focused,
                                    diagnostic: Boolean(line.diagnostic),
                                    changed: Boolean(line.git),
                                }}
                                style={{
                                    top: `${props.pane.totalLines <= 1 ? 0 : ((line.number - 1) / (props.pane.totalLines - 1)) * 100}%`,
                                }}
                            />
                        </Show>
                    )}
                </For>
            </div>
        </section>
    );

    const PaneTree = (props: { node: GuiLayoutNode }) => (
        <Show
            when={props.node.kind === "split" ? props.node : undefined}
            fallback={
                <PaneView
                    pane={
                        view().panes.find(
                            (pane) =>
                                pane.index ===
                                (props.node.kind === "pane"
                                    ? props.node.pane
                                    : 0),
                        ) || view().panes[0]
                    }
                />
            }
        >
            {(split) => (
                <div
                    class={`split-layout ${split().direction}`}
                    style={
                        split().direction === "vertical"
                            ? {
                                  "grid-template-columns": `${split().ratio}fr 1px ${1 - split().ratio}fr`,
                              }
                            : {
                                  "grid-template-rows": `${split().ratio}fr 1px ${1 - split().ratio}fr`,
                              }
                    }
                >
                    <PaneTree node={split().first} />
                    <div class="split-separator" />
                    <PaneTree node={split().second} />
                </div>
            )}
        </Show>
    );

    const AiPanel = () => (
        <Show when={view().aiChat}>
            {(chat) => (
                <ChatPanel
                    chat={chat()}
                    revision={view().revision}
                    focusInput={focusChatInput}
                    bindInput={(input) => {
                        chatInput = input;
                    }}
                    onSetupKey={(key) =>
                        void sendKey({
                            key,
                            shift: false,
                            control: false,
                            alt: false,
                            meta: false,
                        }).finally(() => focusChatInput())
                    }
                    onInputUpdate={(update) =>
                        mutateStrict("gui_update_chat_input", { ...update })
                    }
                    onInputWidth={(columns) =>
                        void mutate("gui_set_chat_input_width", { columns })
                    }
                    onRemoveImage={(index) =>
                        void mutate("gui_remove_chat_image", { index })
                    }
                    onProfile={(profile) =>
                        void mutate("gui_select_ai_profile", { profile })
                    }
                    onReasoningEffort={(effort) =>
                        void mutate("gui_select_reasoning_effort", { effort })
                    }
                    onYolo={() =>
                        void mutate("gui_ai_policy", { action: "toggle-yolo" })
                    }
                    onComprehension={() =>
                        void mutate("gui_ai_policy", {
                            action: "toggle-comprehension",
                        })
                    }
                    onMessage={(index) =>
                        void mutate("gui_select_chat_message", { index })
                    }
                    onAgent={(agentId) =>
                        void mutate("gui_select_chat_agent", { agentId })
                    }
                    onQueuedAction={(id, action) => {
                        void mutate("gui_manage_queued_chat_input", {
                            id,
                            action,
                        });
                        if (action === "recall") queueMicrotask(focusChatInput);
                    }}
                />
            )}
        </Show>
    );

    const TestPanel = () => (
        <Show when={view().testPanel}>
            {(test) => (
                <section
                    class="side-panel test-panel"
                    aria-label="Test output"
                    aria-busy={test().status === "running"}
                >
                    <header class="side-panel-header">
                        <div>
                            <b>{test().scope} tests</b>
                            <small>{test().directory}</small>
                        </div>
                        <span
                            class={`run-status ${test().status}`}
                            role="status"
                            aria-live="polite"
                        >
                            {test().status} ·{" "}
                            {(test().elapsedMs / 1000).toFixed(1)}s
                        </span>
                    </header>
                    <div class="run-command">$ {test().command}</div>
                    <pre class="output-lines">
                        <Show when={test().truncated}>
                            <i>… {test().truncated} earlier lines</i>
                        </Show>
                        <For
                            each={test().lines}
                            fallback={
                                <span class="output-empty">
                                    No test output yet
                                </span>
                            }
                        >
                            {(line) => <span>{line}</span>}
                        </For>
                    </pre>
                    <footer class="panel-summary">
                        <span>{test().summary || "Output updates live"}</span>
                        <div>
                            <button
                                type="button"
                                disabled={test().status === "running"}
                                title="Rerun last test · Space T L"
                                onClick={() => void runEditorShortcut(" tl")}
                            >
                                Rerun
                            </button>
                            <button
                                type="button"
                                title="Open full output · Space T O"
                                onClick={() => void runEditorShortcut(" to")}
                            >
                                Full output
                            </button>
                        </div>
                    </footer>
                </section>
            )}
        </Show>
    );

    const DebugPanel = () => (
        <Show when={view().debug}>
            {(debug) => (
                <section class="side-panel debug-panel" aria-label="Debugger">
                    <header class="side-panel-header">
                        <div>
                            <b>Debugger</b>
                            <small>
                                {debug().reason ||
                                    (debug().executionLine
                                        ? `stopped at line ${debug().executionLine}`
                                        : "session active")}
                            </small>
                        </div>
                        <span>{debug().running ? "running" : "paused"}</span>
                    </header>
                    <div
                        class="debug-controls"
                        role="toolbar"
                        aria-label="Debug controls"
                    >
                        <button
                            type="button"
                            disabled={debug().running}
                            title="Continue · F5"
                            onClick={() => {
                                void sendKey({
                                    key: "F5",
                                    shift: false,
                                    control: false,
                                    alt: false,
                                    meta: false,
                                });
                                queueMicrotask(focusEditorInput);
                            }}
                        >
                            Continue
                        </button>
                        <button
                            type="button"
                            disabled={debug().running}
                            title="Step over · F10"
                            onClick={() => {
                                void sendKey({
                                    key: "F10",
                                    shift: false,
                                    control: false,
                                    alt: false,
                                    meta: false,
                                });
                                queueMicrotask(focusEditorInput);
                            }}
                        >
                            Step over
                        </button>
                        <button
                            type="button"
                            disabled={debug().running}
                            title="Step into · F11"
                            onClick={() => {
                                void sendKey({
                                    key: "F11",
                                    shift: false,
                                    control: false,
                                    alt: false,
                                    meta: false,
                                });
                                queueMicrotask(focusEditorInput);
                            }}
                        >
                            Step in
                        </button>
                        <button
                            type="button"
                            disabled={debug().running}
                            title="Step out · Shift+F11"
                            onClick={() => {
                                void sendKey({
                                    key: "F11",
                                    shift: true,
                                    control: false,
                                    alt: false,
                                    meta: false,
                                });
                                queueMicrotask(focusEditorInput);
                            }}
                        >
                            Step out
                        </button>
                        <button
                            type="button"
                            class="danger"
                            title="Stop · Space D S"
                            onClick={() => void runEditorShortcut(" ds")}
                        >
                            <Icon name="stop" size={16} />
                            Stop
                        </button>
                    </div>
                    <div
                        class="debug-stack"
                        role="listbox"
                        aria-label="Stack frames"
                    >
                        <For
                            each={debug().stack}
                            fallback={
                                <p class="panel-empty compact">
                                    No stack frames available
                                </p>
                            }
                        >
                            {(frame) => (
                                <button
                                    type="button"
                                    role="option"
                                    aria-selected={frame.selected}
                                    classList={{ selected: frame.selected }}
                                    onClick={() => {
                                        void mutate("gui_select_debug_frame", {
                                            index: debug().stack.indexOf(frame),
                                        });
                                        queueMicrotask(focusEditorInput);
                                    }}
                                >
                                    <b>{frame.name}</b>
                                    <small>
                                        {frame.file}:{frame.line}
                                    </small>
                                </button>
                            )}
                        </For>
                    </div>
                    <pre class="output-lines">
                        <For
                            each={debug().output}
                            fallback={
                                <span class="output-empty">
                                    No debugger output yet
                                </span>
                            }
                        >
                            {(line) => <span>{line}</span>}
                        </For>
                    </pre>
                </section>
            )}
        </Show>
    );

    const DiffPanel = () => (
        <GdiffPanel
            native={native}
            workspace={diffWorkspace()}
            onReview={setGdiff}
        />
    );

    const contextPanels = createMemo<ContextPanelDefinition[]>(
        (previous = []) => {
            if (walkthrough()) return [];
            const panels: ContextPanelDefinition[] = [];
            const chat = view().aiChat;
            const tests = view().testPanel;
            const debug = view().debug;
            if (diffDockOpen()) {
                panels.push({
                    id: "diff",
                    label: "Diff",
                    state: gdiff()?.running
                        ? `${gdiff()?.comments.length ?? 0} notes`
                        : "disconnected",
                    icon: "source-control",
                    component: DiffPanel,
                });
            }
            if (chat) {
                panels.push({
                    id: "ai",
                    label: "AI chat",
                    state: chat.activity.replaceAll("_", " "),
                    icon: "ai-spark",
                    component: AiPanel,
                });
            }
            if (tests) {
                panels.push({
                    id: "tests",
                    label: "Tests",
                    state: tests.status,
                    icon: "test",
                    component: TestPanel,
                });
            }
            if (debug) {
                panels.push({
                    id: "debug",
                    label: "Debug",
                    state: debug.running ? "running" : "paused",
                    icon: "debug",
                    component: DebugPanel,
                });
            }
            return retainProjection(previous, panels);
        },
        [],
    );

    const SideDock = () => (
        <ContextDock
            panels={contextPanels()}
            activePanel={activeContextPanel()}
            onActivePanel={setActiveContextPanel}
        />
    );

    const ProblemPanel = () => (
        <Show when={view().problems}>
            {(problems) => (
                <section
                    class="problem-panel"
                    aria-label={problems().title || "Problems"}
                >
                    <header>
                        <b>{problems().title || problems().kind}</b>
                        <span>{problems().total} items</span>
                    </header>
                    <div role="listbox" aria-label="Problem entries">
                        <For
                            each={problems().items}
                            fallback={
                                <p class="panel-empty compact">
                                    No problems in this list
                                </p>
                            }
                        >
                            {(item) => (
                                <button
                                    type="button"
                                    role="option"
                                    aria-selected={
                                        item.index === problems().selected
                                    }
                                    classList={{
                                        selected:
                                            item.index === problems().selected,
                                    }}
                                    onClick={() => {
                                        void mutate("gui_select_problem", {
                                            kind: problems().kind,
                                            index: item.index,
                                            activate: false,
                                        });
                                        queueMicrotask(focusEditorInput);
                                    }}
                                    onDblClick={() => {
                                        void mutate("gui_select_problem", {
                                            kind: problems().kind,
                                            index: item.index,
                                            activate: true,
                                        });
                                        queueMicrotask(focusEditorInput);
                                    }}
                                    onKeyDown={(event) => {
                                        if (event.key !== "Enter") return;
                                        event.preventDefault();
                                        void mutate("gui_select_problem", {
                                            kind: problems().kind,
                                            index: item.index,
                                            activate: true,
                                        });
                                        queueMicrotask(focusEditorInput);
                                    }}
                                >
                                    {(() => {
                                        const status = diagnosticIcon(
                                            item.severity,
                                        );
                                        return (
                                            <Icon
                                                name={status.name}
                                                tone={status.tone}
                                                size={16}
                                            />
                                        );
                                    })()}
                                    <strong>{item.message}</strong>
                                    <small>
                                        {item.file}:{item.line}:{item.column}
                                    </small>
                                </button>
                            )}
                        </For>
                    </div>
                </section>
            )}
        </Show>
    );

    const LspOverlay = () => (
        <Show when={!view().aiChat ? view().lspManager : undefined}>
            {(manager) => {
                const selected = () =>
                    manager().items.find(
                        (item) => item.index === manager().selected,
                    );
                const refocus = () =>
                    queueMicrotask(() =>
                        lspDialog?.focus({ preventScroll: true }),
                    );
                return (
                    <div class="overlay-shade lsp-overlay">
                        <section
                            ref={(element) => {
                                lspDialog = element;
                                queueMicrotask(() =>
                                    element.focus({ preventScroll: true }),
                                );
                            }}
                            class="lsp-panel"
                            role="dialog"
                            aria-labelledby="lsp-manager-title"
                            data-gui-core-dialog
                            tabIndex={-1}
                            onKeyDown={(event) =>
                                void trapDialogFocus(event, event.currentTarget)
                            }
                        >
                            <header>
                                <div>
                                    <b id="lsp-manager-title">
                                        Language servers
                                    </b>
                                    <small>
                                        Install, inspect, and manage language
                                        intelligence
                                    </small>
                                </div>
                                <div class="lsp-header-actions">
                                    <button
                                        type="button"
                                        aria-pressed={manager().showDetail}
                                        onClick={() => {
                                            void sendKey({
                                                key: "K",
                                                shift: true,
                                                control: false,
                                                alt: false,
                                                meta: false,
                                            });
                                            refocus();
                                        }}
                                    >
                                        Details
                                    </button>
                                    <kbd>esc</kbd>
                                </div>
                            </header>
                            <div class="lsp-filter">
                                <Icon name="search" size={16} />
                                {manager().filter || "Filter languages"}
                            </div>
                            <div
                                class="lsp-content"
                                classList={{
                                    "has-detail": manager().showDetail,
                                }}
                            >
                                <div
                                    class="lsp-list"
                                    role="listbox"
                                    aria-label="Language servers"
                                >
                                    <For
                                        each={manager().items}
                                        fallback={
                                            <p class="lsp-empty">
                                                No matching language servers
                                            </p>
                                        }
                                    >
                                        {(item) => (
                                            <button
                                                type="button"
                                                role="option"
                                                aria-selected={
                                                    item.index ===
                                                    manager().selected
                                                }
                                                classList={{
                                                    selected:
                                                        item.index ===
                                                        manager().selected,
                                                }}
                                                onClick={() => {
                                                    void mutate(
                                                        "gui_select_lsp",
                                                        {
                                                            index: item.index,
                                                            activate: false,
                                                        },
                                                    );
                                                    refocus();
                                                }}
                                                onDblClick={() => {
                                                    void mutate(
                                                        "gui_select_lsp",
                                                        {
                                                            index: item.index,
                                                            activate: true,
                                                        },
                                                    );
                                                    refocus();
                                                }}
                                                onKeyDown={(event) => {
                                                    if (event.key !== "Enter")
                                                        return;
                                                    event.preventDefault();
                                                    void mutate(
                                                        "gui_select_lsp",
                                                        {
                                                            index: item.index,
                                                            activate: true,
                                                        },
                                                    );
                                                    refocus();
                                                }}
                                            >
                                                <span
                                                    class={`server-dot ${item.section.toLowerCase().replaceAll(" ", "-")}`}
                                                />
                                                <strong>{item.language}</strong>
                                                <small>
                                                    {item.command ||
                                                        "syntax highlighting"}
                                                </small>
                                                <em>
                                                    {item.installing ||
                                                        item.state ||
                                                        item.section}
                                                </em>
                                            </button>
                                        )}
                                    </For>
                                </div>
                                <Show when={manager().showDetail && selected()}>
                                    {(item) => (
                                        <aside
                                            class="lsp-detail"
                                            aria-label={`${item().language} details`}
                                        >
                                            <header>
                                                <small>{item().section}</small>
                                                <b>{item().language}</b>
                                                <span>
                                                    {item().installing ||
                                                        item().state ||
                                                        "Not running"}
                                                </span>
                                            </header>
                                            <dl>
                                                <Show when={item().command}>
                                                    <div>
                                                        <dt>Command</dt>
                                                        <dd>
                                                            {item().command}
                                                        </dd>
                                                    </div>
                                                </Show>
                                                <Show
                                                    when={
                                                        item().extensions.length
                                                    }
                                                >
                                                    <div>
                                                        <dt>Extensions</dt>
                                                        <dd>
                                                            {item().extensions.join(
                                                                ", ",
                                                            )}
                                                        </dd>
                                                    </div>
                                                </Show>
                                                <Show
                                                    when={
                                                        item().rootMarkers
                                                            .length
                                                    }
                                                >
                                                    <div>
                                                        <dt>Project markers</dt>
                                                        <dd>
                                                            {item().rootMarkers.join(
                                                                ", ",
                                                            )}
                                                        </dd>
                                                    </div>
                                                </Show>
                                                <Show
                                                    when={
                                                        item().capabilities
                                                            .length
                                                    }
                                                >
                                                    <div>
                                                        <dt>Capabilities</dt>
                                                        <dd>
                                                            {item().capabilities.join(
                                                                ", ",
                                                            )}
                                                        </dd>
                                                    </div>
                                                </Show>
                                                <Show when={item().installHint}>
                                                    <div>
                                                        <dt>Install</dt>
                                                        <dd>
                                                            {item().installHint}
                                                        </dd>
                                                    </div>
                                                </Show>
                                            </dl>
                                        </aside>
                                    )}
                                </Show>
                            </div>
                        </section>
                    </div>
                );
            }}
        </Show>
    );

    onMount(() => {
        const canvas = document.createElement("canvas");
        const context = canvas.getContext("2d");
        if (context) {
            context.font =
                getComputedStyle(document.documentElement).getPropertyValue(
                    "--editor-font",
                ) || "13.5px monospace";
            cellWidth = context.measureText("M").width || FALLBACK_CELL_WIDTH;
        }
        window.addEventListener("keydown", handleKeyDown, { capture: true });
        window.addEventListener("paste", handlePaste);
        window.addEventListener("copy", handleCopy);
        window.addEventListener("cut", handleCut);
        const restoreInputFocus = () => {
            // Switching from the child webview to its toolbar focuses the main
            // webview before WebKit assigns focus to the clicked control. Wait
            // for that transition so we do not immediately steal focus back.
            queueMicrotask(() => {
                if (
                    !pendingExit() &&
                    !isGuiNativeControl(document.activeElement, inputSink)
                )
                    focusPrimaryInput();
            });
        };
        window.addEventListener("focus", restoreInputFocus);
        const updateCompactDocks = (event: MediaQueryListEvent) =>
            setCompactDocks(event.matches);
        compactDockQuery?.addEventListener("change", updateCompactDocks);
        editorBody.addEventListener("wheel", handleWheel, { passive: false });
        const observer = new ResizeObserver(syncDimensions);
        observer.observe(editorBody);
        let unlistenMenu: (() => void) | undefined;
        let unlistenClose: (() => void) | undefined;
        let unlistenBrowserState: (() => void) | undefined;
        let unlistenBrowserShow: (() => void) | undefined;
        if (native) {
            void listen<string>("ovim://menu-action", (event) =>
                performMenuAction(event.payload),
            ).then((unlisten) => {
                unlistenMenu = unlisten;
            });
            void listen<string>("ovim://close-requested", (event) =>
                requestExit(event.payload === "quit" ? "quit" : "close"),
            ).then((unlisten) => {
                unlistenClose = unlisten;
            });
            void listen<BrowserState>("ovim://browser-state", (event) =>
                acceptBrowserState(event.payload),
            ).then((unlisten) => {
                unlistenBrowserState = unlisten;
            });
            void listen<string>("ovim://browser-show-requested", (event) => {
                setActiveBrowserId(event.payload);
                setWorkbenchView("browser");
                queueMicrotask(focusBrowser);
            }).then((unlisten) => {
                unlistenBrowserShow = unlisten;
            });
            void invoke<BrowserState>("gui_browser_state")
                .then(acceptBrowserState)
                .catch((reason) => setError(String(reason)));
            const snapshots = new Channel<GuiSnapshot>();
            snapshots.onmessage = accept;
            lastDimensions = dimensions();
            void invoke("gui_subscribe", {
                ...lastDimensions,
                onEvent: snapshots,
            }).catch((reason) => setError(String(reason)));
        }
        restoreInputFocus();
        onCleanup(() => {
            window.removeEventListener("keydown", handleKeyDown, {
                capture: true,
            });
            window.removeEventListener("paste", handlePaste);
            window.removeEventListener("copy", handleCopy);
            window.removeEventListener("cut", handleCut);
            window.removeEventListener("focus", restoreInputFocus);
            compactDockQuery?.removeEventListener("change", updateCompactDocks);
            editorBody.removeEventListener("wheel", handleWheel);
            observer.disconnect();
            unlistenMenu?.();
            unlistenClose?.();
            unlistenBrowserState?.();
            unlistenBrowserShow?.();
        });
    });

    return (
        <main
            class="app"
            classList={{ "walkthrough-open": Boolean(walkthrough()) }}
            style={themeVars()}
        >
            <header
                class="titlebar"
                data-tauri-drag-region
                onDblClick={(event) => {
                    if ((event.target as Element).closest(".window-actions"))
                        return;
                    void windowAction("toggle-maximize");
                }}
            >
                <div class="brand" data-tauri-drag-region>
                    <span class="brand-mark">O</span>
                    <span>ovim</span>
                </div>
                <div class="window-title" data-tauri-drag-region>
                    <span title={view().filePath || view().fileName}>
                        {view().fileName}
                        {view().modified ? " •" : ""}
                    </span>
                    <span class="title-project">— {view().projectName}</span>
                </div>
                <div class="window-actions">
                    <IconButton
                        icon="minimize"
                        label="Minimize"
                        onClick={() => void windowAction("minimize")}
                    />
                    <IconButton
                        icon="maximize"
                        label="Maximize or restore"
                        onClick={() => void windowAction("toggle-maximize")}
                    />
                    <IconButton
                        class="window-close"
                        icon="close"
                        label="Close"
                        onClick={() => void windowAction("close")}
                    />
                </div>
            </header>

            <section
                class="workbench"
                classList={{
                    "active-explorer-dock": activeDock() === "explorer",
                    "active-context-dock": activeDock() === "context",
                }}
            >
                <nav class="activity-bar" aria-label="Primary navigation">
                    <div class="activity-main">
                        <IconButton
                            icon="explorer"
                            label="Explorer"
                            shortcut="-"
                            selected={
                                Boolean(view().fileTree) &&
                                activeDock() === "explorer"
                            }
                            onClick={toggleExplorer}
                        />
                        <IconButton
                            icon="search"
                            label="Search project"
                            shortcut="Space S G"
                            onClick={() => runEditorShortcut(" sg")}
                        />
                        <IconButton
                            icon="source-control"
                            label="Diff collaboration"
                            shortcut="Gdiff"
                            selected={
                                diffDockOpen() &&
                                activeDock() === "context" &&
                                activeContextPanel() === "diff"
                            }
                            onClick={toggleDiff}
                        />
                        <IconButton
                            icon="ai-spark"
                            label="AI chat"
                            shortcut="Space Space"
                            selected={
                                Boolean(view().aiChat) &&
                                activeDock() === "context"
                            }
                            onClick={toggleAiChat}
                        />
                    </div>
                    <IconButton
                        icon="settings"
                        label="Settings"
                        shortcut=":set"
                        onClick={() => runEditorShortcut(":set")}
                    />
                </nav>

                <Show when={view().fileTree}>
                    {(tree) => (
                        <aside class="explorer">
                            <div class="panel-heading">
                                <span>Explorer</span>
                                <small>{tree().root}</small>
                            </div>
                            <div
                                class="tree-list"
                                role="tree"
                                aria-label="Project files"
                            >
                                <For
                                    each={tree().items}
                                    fallback={
                                        <p class="panel-empty compact">
                                            This workspace is empty
                                        </p>
                                    }
                                >
                                    {(item) => (
                                        <button
                                            type="button"
                                            role="treeitem"
                                            aria-selected={
                                                item.index === tree().selected
                                            }
                                            aria-expanded={
                                                item.directory
                                                    ? item.expanded
                                                    : undefined
                                            }
                                            aria-level={item.depth + 1}
                                            class="tree-item"
                                            classList={{
                                                selected:
                                                    item.index ===
                                                    tree().selected,
                                            }}
                                            style={{
                                                "padding-left": `${10 + item.depth * 14}px`,
                                            }}
                                            title={item.path}
                                            onClick={() => {
                                                void mutate(
                                                    "gui_select_file_tree",
                                                    {
                                                        index: item.index,
                                                        activate: false,
                                                    },
                                                );
                                                queueMicrotask(
                                                    focusEditorInput,
                                                );
                                            }}
                                            onDblClick={() => {
                                                void mutate(
                                                    "gui_select_file_tree",
                                                    {
                                                        index: item.index,
                                                        activate: true,
                                                    },
                                                );
                                                queueMicrotask(
                                                    focusEditorInput,
                                                );
                                            }}
                                            onKeyDown={(event) => {
                                                if (event.key !== "Enter")
                                                    return;
                                                event.preventDefault();
                                                void mutate(
                                                    "gui_select_file_tree",
                                                    {
                                                        index: item.index,
                                                        activate: true,
                                                    },
                                                );
                                                queueMicrotask(
                                                    focusEditorInput,
                                                );
                                            }}
                                        >
                                            <span
                                                class={`tree-chevron ${item.directory ? "directory" : "file"}`}
                                            >
                                                <Show when={item.directory}>
                                                    <Icon
                                                        name={
                                                            item.expanded
                                                                ? "chevron-down"
                                                                : "chevron-right"
                                                        }
                                                        size={16}
                                                    />
                                                </Show>
                                            </span>
                                            <Icon
                                                name={
                                                    item.directory
                                                        ? "folder"
                                                        : "file"
                                                }
                                                size={16}
                                                tone={
                                                    item.directory
                                                        ? "warning"
                                                        : "muted"
                                                }
                                            />
                                            <span>{item.name}</span>
                                        </button>
                                    )}
                                </For>
                            </div>
                        </aside>
                    )}
                </Show>

                <section class="editor-stack">
                    <div class="tabs" role="tablist" aria-label="Open files">
                        <Index each={view().tabs}>
                            {(tab, position) => (
                                <button
                                    type="button"
                                    role="tab"
                                    aria-selected={
                                        tab().active &&
                                        workbenchView() === "source"
                                    }
                                    aria-controls="editor-surface"
                                    tabIndex={
                                        tab().active &&
                                        workbenchView() === "source"
                                            ? 0
                                            : -1
                                    }
                                    data-tab-index={tab().index}
                                    data-workbench-tab-index={position}
                                    class="tab"
                                    classList={{
                                        active:
                                            tab().active &&
                                            workbenchView() === "source",
                                    }}
                                    aria-label={
                                        tab().title +
                                        (tab().modified ? ", modified" : "")
                                    }
                                    title={
                                        tab().title +
                                        (tab().modified ? " · modified" : "")
                                    }
                                    onClick={() => {
                                        selectWorkbenchTab(position);
                                        queueMicrotask(focusEditorInput);
                                    }}
                                    onKeyDown={(event) =>
                                        handleTabNavigation(event, position)
                                    }
                                >
                                    <Icon
                                        name="file"
                                        size={16}
                                        tone={tab().active ? "accent" : "muted"}
                                    />
                                    <span>{tab().title}</span>
                                    <Show when={tab().modified}>
                                        <span class="modified-dot" />
                                    </Show>
                                </button>
                            )}
                        </Index>
                        <Show when={activeStrok()}>
                            <button
                                type="button"
                                role="tab"
                                aria-selected={workbenchView() === "vector"}
                                aria-controls="editor-surface"
                                tabIndex={workbenchView() === "vector" ? 0 : -1}
                                data-workbench-tab-index={view().tabs.length}
                                class="tab vector-tab"
                                classList={{
                                    active: workbenchView() === "vector",
                                }}
                                title="Live Strøk render and review"
                                onClick={() => setWorkbenchView("vector")}
                                onKeyDown={(event) =>
                                    handleTabNavigation(
                                        event,
                                        view().tabs.length,
                                    )
                                }
                            >
                                <Icon
                                    name="ai-spark"
                                    size={16}
                                    tone={
                                        workbenchView() === "vector"
                                            ? "accent"
                                            : "muted"
                                    }
                                />
                                <span>Vector</span>
                            </button>
                        </Show>
                        <For each={browserState().sessions}>
                            {(session, index) => {
                                const position = () =>
                                    view().tabs.length +
                                    (activeStrok() ? 1 : 0) +
                                    index();
                                const active = () =>
                                    workbenchView() === "browser" &&
                                    activeBrowserId() === session.sessionId;
                                const title = () => browserTabTitle(session);
                                return (
                                    <button
                                        type="button"
                                        role="tab"
                                        aria-selected={active()}
                                        aria-controls="editor-surface"
                                        aria-label={`Browser: ${title()}`}
                                        tabIndex={active() ? 0 : -1}
                                        data-workbench-tab-index={position()}
                                        class="tab browser-tab"
                                        classList={{ active: active() }}
                                        title={`${title()} · ${session.url}`}
                                        onClick={() =>
                                            activateBrowserSession(
                                                session.sessionId,
                                            )
                                        }
                                        onKeyDown={(event) =>
                                            handleTabNavigation(
                                                event,
                                                position(),
                                            )
                                        }
                                    >
                                        <Icon
                                            name="search"
                                            size={16}
                                            tone={active() ? "accent" : "muted"}
                                        />
                                        <span>{title()}</span>
                                        <Show when={session.loading}>
                                            <span
                                                class="browser-tab-loading"
                                                aria-label="Loading"
                                            />
                                        </Show>
                                    </button>
                                );
                            }}
                        </For>
                        <button
                            type="button"
                            class="new-browser-tab"
                            data-gui-native-control
                            disabled={
                                !native ||
                                browserOpening() ||
                                browserState().sessions.length >=
                                    browserState().maxSessions
                            }
                            aria-label="New browser tab"
                            title={
                                native
                                    ? browserState().sessions.length >=
                                      browserState().maxSessions
                                        ? `Browser tab limit (${browserState().maxSessions}) reached`
                                        : "New browser tab"
                                    : "Browser tabs require the native desktop app"
                            }
                            onClick={() => void openBrowserSession()}
                        >
                            <span aria-hidden="true">+</span>
                        </button>
                        <span class="tabs-fill" role="presentation" />
                    </div>

                    <div class="breadcrumbs">
                        <Show
                            when={workbenchView() === "browser"}
                            fallback={
                                <>
                                    <For each={breadcrumbs()}>
                                        {(part, index) => (
                                            <>
                                                <span>{part}</span>
                                                <Show
                                                    when={
                                                        index() <
                                                        breadcrumbs().length - 1
                                                    }
                                                >
                                                    <Icon
                                                        name="chevron-right"
                                                        size={16}
                                                        tone="muted"
                                                    />
                                                </Show>
                                            </>
                                        )}
                                    </For>
                                    <Show when={view().readOnly}>
                                        <span class="readonly">read only</span>
                                    </Show>
                                </>
                            }
                        >
                            <span>Ovim</span>
                            <Icon name="chevron-right" size={16} tone="muted" />
                            <span>
                                {activeBrowser()
                                    ? browserTabTitle(activeBrowser()!)
                                    : "Browser"}
                            </span>
                            <span class="browser-breadcrumb-note">
                                isolated · shared with agent
                            </span>
                        </Show>
                    </div>

                    <div
                        id="editor-surface"
                        class="editor-body"
                        classList={{
                            "vector-view-active":
                                activeStrok() && workbenchView() === "vector",
                            "browser-view-active":
                                workbenchView() === "browser",
                        }}
                        ref={editorBody!}
                    >
                        <Show
                            when={activeStrok() && workbenchView() === "vector"}
                        >
                            <section
                                class="vector-workbench"
                                aria-label="Strøk vector preview"
                            >
                                <header class="vector-toolbar">
                                    <div>
                                        <strong>{view().fileName}</strong>
                                        <span>in-memory Strøk preview</span>
                                    </div>
                                    <button
                                        type="button"
                                        data-gui-native-control
                                        disabled={vectorPreviewLoading()}
                                        onClick={() =>
                                            setVectorRefresh(
                                                (revision) => revision + 1,
                                            )
                                        }
                                    >
                                        {vectorPreviewLoading()
                                            ? "Rendering…"
                                            : "Refresh"}
                                    </button>
                                </header>
                                <div class="vector-canvas">
                                    <Show
                                        when={vectorPreview()}
                                        fallback={
                                            <p class="vector-state">
                                                {vectorPreviewLoading()
                                                    ? "Rendering with Strøk…"
                                                    : vectorPreviewError() ||
                                                      "Open the Vector tab to render this document."}
                                            </p>
                                        }
                                    >
                                        {(preview) => (
                                            <img
                                                src={preview().dataUrl}
                                                width={preview().width}
                                                height={preview().height}
                                                alt={`Rendered preview of ${preview().fileName}`}
                                            />
                                        )}
                                    </Show>
                                    <Show
                                        when={
                                            vectorPreviewError() &&
                                            vectorPreview()
                                        }
                                    >
                                        <p class="vector-error" role="alert">
                                            {vectorPreviewError()}
                                        </p>
                                    </Show>
                                </div>
                                <form
                                    class="vector-feedback"
                                    onSubmit={(event) => {
                                        event.preventDefault();
                                        void addVectorFeedbackToChat();
                                    }}
                                >
                                    <label for="vector-feedback-input">
                                        Review with the agent
                                    </label>
                                    <textarea
                                        id="vector-feedback-input"
                                        data-gui-native-control
                                        maxLength={8 * 1024}
                                        value={vectorFeedback()}
                                        placeholder="What should change in this vector?"
                                        onInput={(event) => {
                                            setVectorFeedback(
                                                event.currentTarget.value,
                                            );
                                            setVectorFeedbackStatus("");
                                        }}
                                    />
                                    <button
                                        type="submit"
                                        data-gui-native-control
                                        disabled={!vectorFeedback().trim()}
                                    >
                                        Add to agent chat
                                    </button>
                                    <Show when={vectorFeedbackStatus()}>
                                        <small role="status">
                                            {vectorFeedbackStatus()}
                                        </small>
                                    </Show>
                                </form>
                            </section>
                        </Show>
                        <BrowserPanel
                            native={native}
                            active={workbenchView() === "browser"}
                            session={activeBrowser()}
                            obscured={Boolean(
                                pendingExit() ||
                                view().picker ||
                                view().lspManager,
                            )}
                            onState={acceptBrowserState}
                            onClosed={(sessionId) => {
                                if (activeBrowserId() === sessionId) {
                                    setActiveBrowserId(undefined);
                                    setWorkbenchView("source");
                                    queueMicrotask(focusEditorInput);
                                }
                            }}
                        />
                        <textarea
                            ref={inputSink!}
                            class="input-sink"
                            style={{
                                top: `${Math.max(0, view().cursor.line - view().firstLine) * LINE_HEIGHT + 8}px`,
                                left: `${Math.max(0, view().cursor.displayColumn - view().horizontalOffset) * cellWidth + 66}px`,
                            }}
                            aria-label="Ovim editor input"
                            aria-multiline="true"
                            autocomplete="off"
                            autocapitalize="off"
                            spellcheck={false}
                            onCompositionStart={handleCompositionStart}
                            onCompositionUpdate={handleCompositionUpdate}
                            onCompositionEnd={handleCompositionEnd}
                            onInput={handleTextInput}
                        />
                        <Show when={composition()}>
                            {(text) => (
                                <span
                                    class="ime-preview"
                                    style={{
                                        top: `${Math.max(0, view().cursor.line - view().firstLine) * LINE_HEIGHT + 8}px`,
                                        left: `${Math.max(0, view().cursor.displayColumn - view().horizontalOffset) * cellWidth + 66}px`,
                                    }}
                                >
                                    {text()}
                                </span>
                            )}
                        </Show>
                        <Show
                            when={!view().dashboard}
                            fallback={
                                <Dashboard
                                    send={runEditorShortcut}
                                    version="1.2.7"
                                />
                            }
                        >
                            <div
                                class="editor-content"
                                classList={{
                                    "has-problems": Boolean(view().problems),
                                }}
                            >
                                <div class="primary-content">
                                    <div class="pane-tree">
                                        <PaneTree node={view().layout} />
                                    </div>
                                    <SideDock />
                                </div>
                                <ProblemPanel />
                            </div>
                        </Show>

                        <Show when={walkthrough()}>
                            {(active) => (
                                <CodeWalkthrough
                                    walkthrough={active()}
                                    restoreFocus={focusPrimaryInput}
                                    onKey={(key) =>
                                        void sendKey({
                                            key,
                                            shift: false,
                                            control: false,
                                            alt: false,
                                            meta: false,
                                        })
                                    }
                                />
                            )}
                        </Show>

                        <Show
                            when={
                                !view().aiChat ? view().completion : undefined
                            }
                        >
                            {(menu) => (
                                <div
                                    class="completion-popover"
                                    role="listbox"
                                    aria-label="Code completions"
                                    style={inlineOverlayStyle(
                                        view().cursor.line,
                                        view().cursor.displayColumn,
                                        430,
                                        Math.min(
                                            290,
                                            Math.max(
                                                34,
                                                menu().items.length * 34,
                                            ),
                                        ),
                                    )}
                                >
                                    <For
                                        each={menu().items}
                                        fallback={
                                            <p class="panel-empty compact">
                                                No completions available
                                            </p>
                                        }
                                    >
                                        {(item) => (
                                            <button
                                                type="button"
                                                role="option"
                                                aria-selected={
                                                    item.index ===
                                                    menu().selected
                                                }
                                                class="completion-item"
                                                classList={{
                                                    selected:
                                                        item.index ===
                                                        menu().selected,
                                                }}
                                                onPointerEnter={() =>
                                                    void mutate(
                                                        "gui_select_completion",
                                                        {
                                                            index: item.index,
                                                            activate: false,
                                                        },
                                                    )
                                                }
                                                onClick={() => {
                                                    void mutate(
                                                        "gui_select_completion",
                                                        {
                                                            index: item.index,
                                                            activate: true,
                                                        },
                                                    );
                                                    queueMicrotask(
                                                        focusEditorInput,
                                                    );
                                                }}
                                            >
                                                <span class="completion-kind">
                                                    <Icon
                                                        name="command"
                                                        size={16}
                                                    />
                                                </span>
                                                <strong>{item.label}</strong>
                                                <small>{item.detail}</small>
                                            </button>
                                        )}
                                    </For>
                                </div>
                            )}
                        </Show>

                        <Show when={!view().aiChat ? view().hover : undefined}>
                            {(hover) => (
                                <section
                                    class="hover-popover"
                                    aria-label="Documentation"
                                    style={inlineOverlayStyle(
                                        hover().line ?? view().cursor.line,
                                        hover().displayColumn ??
                                            view().cursor.displayColumn,
                                        520,
                                        340,
                                    )}
                                >
                                    <div class="popover-label">
                                        Documentation
                                    </div>
                                    <Markdown text={hover().content} />
                                </section>
                            )}
                        </Show>

                        <Show when={!view().aiChat ? view().picker : undefined}>
                            {(picker) => (
                                <div class="overlay-shade">
                                    <section
                                        ref={(element) =>
                                            queueMicrotask(() =>
                                                element.focus({
                                                    preventScroll: true,
                                                }),
                                            )
                                        }
                                        class="picker"
                                        role="dialog"
                                        aria-labelledby="picker-title"
                                        data-gui-core-dialog
                                        tabIndex={-1}
                                        onKeyDown={(event) =>
                                            void trapDialogFocus(
                                                event,
                                                event.currentTarget,
                                            )
                                        }
                                    >
                                        <header>
                                            <Icon name="search" />
                                            <span id="picker-title">
                                                {picker().query ||
                                                    picker().title}
                                            </span>
                                            <kbd>esc</kbd>
                                        </header>
                                        <Show when={picker().fileFilter}>
                                            <div class="picker-filter">
                                                in{" "}
                                                <strong>
                                                    {picker().fileFilter}
                                                </strong>
                                            </div>
                                        </Show>
                                        <div
                                            class="picker-results"
                                            role="listbox"
                                            aria-label="Command results"
                                        >
                                            <For
                                                each={picker().items}
                                                fallback={
                                                    <p class="panel-empty compact">
                                                        No matching results
                                                    </p>
                                                }
                                            >
                                                {(item) => (
                                                    <button
                                                        type="button"
                                                        role="option"
                                                        aria-selected={
                                                            item.index ===
                                                            picker().selected
                                                        }
                                                        classList={{
                                                            selected:
                                                                item.index ===
                                                                picker()
                                                                    .selected,
                                                        }}
                                                        onClick={() =>
                                                            void mutate(
                                                                "gui_select_picker",
                                                                {
                                                                    index: item.index,
                                                                },
                                                            ).finally(
                                                                focusEditorInput,
                                                            )
                                                        }
                                                    >
                                                        <span class="picker-icon">
                                                            <Icon
                                                                name="file"
                                                                size={16}
                                                            />
                                                        </span>
                                                        <span class="picker-copy">
                                                            <strong>
                                                                <For
                                                                    each={pickerChars(
                                                                        item.display,
                                                                        item.matched,
                                                                    )}
                                                                >
                                                                    {(part) => (
                                                                        <span
                                                                            classList={{
                                                                                matched:
                                                                                    part.matched,
                                                                            }}
                                                                        >
                                                                            {
                                                                                part.char
                                                                            }
                                                                        </span>
                                                                    )}
                                                                </For>
                                                            </strong>
                                                            <small>
                                                                {item.detail ||
                                                                    item.location}
                                                            </small>
                                                        </span>
                                                    </button>
                                                )}
                                            </For>
                                        </div>
                                        <footer>
                                            <span>
                                                {picker().total} results
                                            </span>
                                            <span>
                                                <kbd>↑↓</kbd> navigate{" "}
                                                <kbd>↵</kbd> open
                                            </span>
                                        </footer>
                                    </section>
                                </div>
                            )}
                        </Show>
                        <LspOverlay />
                    </div>

                    <div class="message-line">
                        <Show
                            when={view().prompt}
                            fallback={
                                <span
                                    class="message"
                                    role={error() ? "alert" : "status"}
                                    aria-live={error() ? "assertive" : "polite"}
                                    aria-atomic="true"
                                >
                                    {error() ||
                                        view().statusMessage ||
                                        view().lspStatus}
                                </span>
                            }
                        >
                            {(prompt) => (
                                <div class="prompt">
                                    <b>{prompt().prefix}</b>
                                    <span>{prompt().text}</span>
                                    <i />
                                </div>
                            )}
                        </Show>
                        <Show when={!connected()}>
                            <span
                                class="connecting"
                                role="status"
                                aria-live="polite"
                            >
                                connecting…
                            </span>
                        </Show>
                    </div>

                    <footer class="statusbar">
                        <div class="mode-chip">{view().mode}</div>
                        <div class="status-left">
                            <Show when={view().gitBranch}>
                                <span>
                                    <Icon name="source-control" />
                                    {view().gitBranch}
                                </span>
                            </Show>
                            <span
                                class="git-counts"
                                role="img"
                                aria-label={`${view().gitChanges.added} added, ${view().gitChanges.modified} modified, ${view().gitChanges.removed} removed`}
                            >
                                <b>+{view().gitChanges.added}</b>
                                <i>~{view().gitChanges.modified}</i>
                                <em>−{view().gitChanges.removed}</em>
                            </span>
                            <span
                                classList={{
                                    has: view().diagnostics.errors > 0,
                                }}
                                class="problems"
                                role="img"
                                aria-label={`${view().diagnostics.errors} errors, ${view().diagnostics.warnings} warnings`}
                            >
                                <Icon name="status-error" tone="error" />
                                {view().diagnostics.errors}
                                <Icon name="status-warning" tone="warning" />
                                {view().diagnostics.warnings}
                            </span>
                        </div>
                        <div class="status-right">
                            <span
                                class="status-language"
                                title={"Language: " + view().language}
                            >
                                {view().language}
                            </span>
                            <span
                                class="status-encoding"
                                title={"Encoding: " + view().encoding}
                            >
                                {view().encoding}
                            </span>
                            <span
                                class="status-line-ending"
                                title={"Line ending: " + view().lineEnding}
                            >
                                {view().lineEnding}
                            </span>
                            <span
                                class="status-indentation"
                                title={
                                    (view().expandTab ? "Spaces" : "Tabs") +
                                    ", width " +
                                    view().tabWidth
                                }
                            >
                                {view().expandTab ? "Spaces" : "Tabs"}:{" "}
                                {view().tabWidth}
                            </span>
                            <Show when={view().wrap}>
                                <span class="status-wrap" title="Line wrap on">
                                    Wrap
                                </span>
                            </Show>
                            <span
                                class="status-cursor"
                                title={
                                    "Line " +
                                    (view().cursor.line + 1) +
                                    " of " +
                                    view().totalLines +
                                    ", column " +
                                    (view().cursor.column + 1)
                                }
                            >
                                {view().cursor.line + 1}:
                                {view().cursor.column + 1}
                            </span>
                        </div>
                    </footer>
                </section>
            </section>
            <Show when={pendingExit()}>
                {(kind) => (
                    <div class="exit-confirmation-layer">
                        <section
                            class="exit-confirmation"
                            role="dialog"
                            aria-modal="true"
                            aria-labelledby="exit-confirmation-title"
                            data-gui-core-dialog
                            tabIndex={-1}
                            ref={(dialog) =>
                                queueMicrotask(() =>
                                    dialog.focus({ preventScroll: true }),
                                )
                            }
                            onKeyDown={(event) => {
                                if (event.key === "Escape") {
                                    event.preventDefault();
                                    setPendingExit();
                                    queueMicrotask(focusPrimaryInput);
                                    return;
                                }
                                void trapDialogFocus(
                                    event,
                                    event.currentTarget,
                                );
                            }}
                        >
                            <small>
                                {kind() === "quit"
                                    ? "Quit Ovim"
                                    : "Close window"}
                            </small>
                            <h2 id="exit-confirmation-title">
                                {view().hasUnsavedChanges
                                    ? "Save changes before leaving?"
                                    : "Quit Ovim?"}
                            </h2>
                            <p>
                                {view().hasUnsavedChanges
                                    ? "Unsaved changes exist in one or more buffers."
                                    : "The current editing session will end."}
                            </p>
                            <footer>
                                <button
                                    type="button"
                                    onClick={() => {
                                        setPendingExit();
                                        queueMicrotask(focusPrimaryInput);
                                    }}
                                >
                                    Cancel
                                </button>
                                <Show when={view().hasUnsavedChanges}>
                                    <button
                                        type="button"
                                        onClick={() => void saveAndExit()}
                                    >
                                        Save All and{" "}
                                        {kind() === "quit" ? "Quit" : "Close"}
                                    </button>
                                </Show>
                                <button
                                    type="button"
                                    class="danger"
                                    onClick={discardAndExit}
                                >
                                    {view().hasUnsavedChanges
                                        ? `${kind() === "quit" ? "Quit" : "Close"} Without Saving`
                                        : "Quit"}
                                </button>
                            </footer>
                        </section>
                    </div>
                )}
            </Show>
            <div class="minimum-window-notice" role="status">
                <Icon name="maximize" size={20} />
                <b>More room required</b>
                <span>Expand the window to at least 720 × 560.</span>
            </div>
        </main>
    );
}

function Dashboard(props: {
    send: (keys: string) => Promise<void>;
    version: string;
}) {
    const shortcuts = [
        [" sf", "Find a file"],
        [" sg", "Search the project"],
        ["  ", "Open AI chat"],
        [" tn", "Run nearest test"],
        [" ca", "Code actions"],
        ["gd", "Jump to definition"],
        ["K", "Hover docs"],
    ];
    return (
        <section class="dashboard">
            <div class="dashboard-logo">
                <span>O</span>
                <div>
                    <strong>ovim</strong>
                    <small>oxidized, now native</small>
                </div>
            </div>
            <div class="dashboard-rule" />
            <div class="dashboard-shortcuts">
                <For each={shortcuts}>
                    {([keys, label]) => (
                        <button
                            type="button"
                            onClick={() => void props.send(keys)}
                        >
                            <kbd>{keys.replaceAll(" ", "␠")}</kbd>
                            <span>{label}</span>
                        </button>
                    )}
                </For>
            </div>
            <p>
                Vim semantics · tree-sitter · LSP · AI <b>v{props.version}</b>
            </p>
        </section>
    );
}

export default App;
