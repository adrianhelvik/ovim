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
import DOMPurify from "dompurify";
import { marked } from "marked";
import { mockSnapshot } from "./mock";
import ChatModelPicker from "./ChatModelPicker";
import ChatComposer, { type ChatInputUpdate } from "./ChatComposer";
import ContextDock, { type ContextPanelDefinition } from "./ContextDock";
import { guiKeyInput } from "./guiInput";
import { Icon, IconButton, type IconTone } from "./Icon";
import { themeVariables } from "./theme";
import { splitAtUtf8Offset } from "./textEncoding";
import { trapDialogFocus } from "./focus";
import type {
    GuiAiChat,
    GuiCodeExplanation,
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

export const Markdown = (props: { text: string }) => {
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

    return <div class="markdown" innerHTML={html()} />;
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
                            disabled={
                                props.walkthrough.current === 1 || composing()
                            }
                            onClick={() => dispatch("ArrowLeft")}
                        >
                            Previous
                        </button>
                        <button
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
                            disabled={answering()}
                            onClick={() =>
                                dispatch(composing() ? "Escape" : " ")
                            }
                        >
                            {composing() ? "Cancel question" : "Ask a question"}
                        </button>
                        <button
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
                <small>{props.setup.error}</small>
            </Show>
            <footer>
                <For each={props.setup.actions}>
                    {(action) => (
                        <button onClick={() => props.onKey?.(action.key)}>
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
    const transcriptItems = createMemo(() =>
        chatTranscriptItems(
            props.chat.messages,
            props.chat.streamingThinking,
            props.chat.thinkingLive,
            props.chat.activity !== "idle",
        ),
    );

    createEffect(() => {
        const selected = props.chat.messages.find(
            (message) => message.selected,
        )?.id;
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
        const revision = `${messages.length}:${latest?.content.length ?? 0}:${props.chat.streaming?.length ?? 0}:${props.chat.streamingThinking?.length ?? 0}:${queued.length}:${queued.at(-1)?.content.length ?? 0}:${props.chat.approval?.length ?? 0}`;
        if (messages.length > messageCount && latest?.role === "user")
            setFollowing(true);
        messageCount = messages.length;
        void revision;
        queueMicrotask(() => {
            if (following()) jumpToLatest();
        });
    });

    return (
        <section class="side-panel ai-panel" aria-label="AI chat">
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
                <span classList={{ working: props.chat.activity !== "idle" }}>
                    {props.chat.activity.replaceAll("_", " ")}
                </span>
            </header>
            <Show when={props.chat.agents.length}>
                <section class="chat-agents" aria-label="Agent navigation">
                    <button
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
                                <span>
                                    <b>{agent.taskName}</b>
                                    <small>{agent.model}</small>
                                </span>
                                <em>{agent.lifecycle.replaceAll("_", " ")}</em>
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
                    <div class="approval-card">
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
    const [compactDocks, setCompactDocks] = createSignal(
        compactDockQuery?.matches ?? false,
    );
    const [activeDock, setActiveDock] = createSignal<"explorer" | "context">(
        mockSnapshot.aiChat || mockSnapshot.testPanel || mockSnapshot.debug
            ? "context"
            : "explorer",
    );
    const [activeContextPanel, setActiveContextPanel] = createSignal<
        "ai" | "tests" | "debug"
    >("ai");
    let editorBody!: HTMLDivElement;
    let inputSink!: HTMLTextAreaElement;
    let chatInput: HTMLTextAreaElement | undefined;
    let lspDialog: HTMLElement | undefined;
    let cellWidth = FALLBACK_CELL_WIDTH;
    let composing = false;
    let ignoreNextInput = false;
    let wheelRemainder = 0;
    let lastDimensions = { columns: 0, rows: 0 };
    const walkthrough = createMemo(() => view().aiChat?.codeExplanation);
    const hasContextDock = createMemo(() =>
        Boolean(
            !walkthrough() &&
            (view().aiChat || view().testPanel || view().debug),
        ),
    );
    let hadContextDock = hasContextDock();
    let previousContextAvailability = {
        ai: Boolean(view().aiChat),
        tests: Boolean(view().testPanel),
        debug: Boolean(view().debug),
    };

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
        };
        if (next.ai && !previousContextAvailability.ai)
            setActiveContextPanel("ai");
        if (next.tests && !previousContextAvailability.tests)
            setActiveContextPanel("tests");
        if (next.debug && !previousContextAvailability.debug)
            setActiveContextPanel("debug");
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
        const chatOpened = !view().aiChat && Boolean(snapshot.aiChat);
        const chatClosed = Boolean(view().aiChat) && !snapshot.aiChat;
        const coreDialogClosed =
            Boolean(view().picker || view().lspManager) &&
            !snapshot.picker &&
            !snapshot.lspManager;
        setView(snapshot);
        setConnected(true);
        setError("");
        requestAnimationFrame(syncDimensions);
        if (chatOpened) queueMicrotask(focusChatInput);
        if (chatClosed) queueMicrotask(focusEditorInput);
        if (coreDialogClosed) queueMicrotask(focusEditorInput);
        if (snapshot.shouldQuit && native) void windowAction("close");
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
    const focusPrimaryInput = () => {
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
    const windowAction = (action: string) =>
        invoke<void>("gui_window_action", { action });

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

    const runEditorShortcut = async (keys: string) => {
        focusEditorInput();
        await sendLiteral(keys);
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
        if (event.key === "Tab" && target?.closest?.("[data-gui-core-dialog]"))
            return;
        if (
            target !== inputSink &&
            target?.closest?.(
                "button, input, select, textarea, [contenteditable='true'], [data-gui-native-control]",
            )
        )
            return;
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

    const PaneView = (props: { pane: GuiPane }) => (
        <section
            class="editor-pane"
            data-pane={props.pane.index}
            classList={{
                focused: props.pane.focused,
                single: view().panes.length === 1,
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
            <div class="overview-ruler">
                <For each={props.pane.lines}>
                    {(line) => (
                        <span
                            classList={{
                                current: line.current && props.pane.focused,
                                diagnostic: Boolean(line.diagnostic),
                                changed: Boolean(line.git),
                            }}
                        />
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
                <section class="side-panel test-panel" aria-label="Test output">
                    <header class="side-panel-header">
                        <div>
                            <b>{test().scope} tests</b>
                            <small>{test().directory}</small>
                        </div>
                        <span class={`run-status ${test().status}`}>
                            {test().status} ·{" "}
                            {(test().elapsedMs / 1000).toFixed(1)}s
                        </span>
                    </header>
                    <div class="run-command">$ {test().command}</div>
                    <pre class="output-lines">
                        <Show when={test().truncated}>
                            <i>… {test().truncated} earlier lines</i>
                        </Show>
                        <For each={test().lines}>
                            {(line) => <span>{line}</span>}
                        </For>
                    </pre>
                    <footer class="panel-summary">
                        {test().summary || "Output updates live"}
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
                            <small>{debug().reason || "session active"}</small>
                        </div>
                        <span>{debug().running ? "running" : "paused"}</span>
                    </header>
                    <div
                        class="debug-stack"
                        role="listbox"
                        aria-label="Stack frames"
                    >
                        <For each={debug().stack}>
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
                        <For each={debug().output}>
                            {(line) => <span>{line}</span>}
                        </For>
                    </pre>
                </section>
            )}
        </Show>
    );

    const contextPanels = createMemo<ContextPanelDefinition[]>(() => {
        if (walkthrough()) return [];
        const panels: ContextPanelDefinition[] = [];
        const chat = view().aiChat;
        const tests = view().testPanel;
        const debug = view().debug;
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
        return panels;
    });

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
                        <For each={problems().items}>
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
            {(manager) => (
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
                                <b id="lsp-manager-title">Language servers</b>
                                <small>
                                    Install, inspect, and manage language
                                    intelligence
                                </small>
                            </div>
                            <kbd>esc</kbd>
                        </header>
                        <div class="lsp-filter">
                            <Icon name="search" size={16} />
                            {manager().filter || "Filter languages"}
                        </div>
                        <div
                            class="lsp-list"
                            role="listbox"
                            aria-label="Language servers"
                        >
                            <For each={manager().items}>
                                {(item) => (
                                    <button
                                        type="button"
                                        role="option"
                                        aria-selected={
                                            item.index === manager().selected
                                        }
                                        classList={{
                                            selected:
                                                item.index ===
                                                manager().selected,
                                        }}
                                        onClick={() => {
                                            void mutate("gui_select_lsp", {
                                                index: item.index,
                                                activate: false,
                                            });
                                            queueMicrotask(() =>
                                                lspDialog?.focus({
                                                    preventScroll: true,
                                                }),
                                            );
                                        }}
                                        onDblClick={() => {
                                            void mutate("gui_select_lsp", {
                                                index: item.index,
                                                activate: true,
                                            });
                                            queueMicrotask(() =>
                                                lspDialog?.focus({
                                                    preventScroll: true,
                                                }),
                                            );
                                        }}
                                        onKeyDown={(event) => {
                                            if (event.key !== "Enter") return;
                                            event.preventDefault();
                                            void mutate("gui_select_lsp", {
                                                index: item.index,
                                                activate: true,
                                            });
                                            queueMicrotask(() =>
                                                lspDialog?.focus({
                                                    preventScroll: true,
                                                }),
                                            );
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
                    </section>
                </div>
            )}
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
        const restoreInputFocus = () => focusPrimaryInput();
        window.addEventListener("focus", restoreInputFocus);
        const updateCompactDocks = (event: MediaQueryListEvent) =>
            setCompactDocks(event.matches);
        compactDockQuery?.addEventListener("change", updateCompactDocks);
        editorBody.addEventListener("wheel", handleWheel, { passive: false });
        const observer = new ResizeObserver(syncDimensions);
        observer.observe(editorBody);
        if (native) {
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
        });
    });

    return (
        <main
            class="app"
            classList={{ "walkthrough-open": Boolean(walkthrough()) }}
            style={themeVars()}
        >
            <header class="titlebar" data-tauri-drag-region>
                <div class="brand" data-tauri-drag-region>
                    <span class="brand-mark">O</span>
                    <span>ovim</span>
                </div>
                <div class="window-title" data-tauri-drag-region>
                    <span>
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
                            label="Source control"
                            shortcut="Unavailable"
                            disabled
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
                                <For each={tree().items}>
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
                        <For each={view().tabs}>
                            {(tab) => (
                                <button
                                    type="button"
                                    role="tab"
                                    aria-selected={tab.active}
                                    aria-controls="editor-surface"
                                    tabIndex={tab.active ? 0 : -1}
                                    class="tab"
                                    classList={{ active: tab.active }}
                                    onClick={() => {
                                        void mutate("gui_select_tab", {
                                            index: tab.index,
                                        });
                                        queueMicrotask(focusEditorInput);
                                    }}
                                >
                                    <Icon
                                        name="file"
                                        size={16}
                                        tone={tab.active ? "accent" : "muted"}
                                    />
                                    <span>{tab.title}</span>
                                    <Show when={tab.modified}>
                                        <span class="modified-dot" />
                                    </Show>
                                </button>
                            )}
                        </For>
                        <span class="tabs-fill" role="presentation" />
                    </div>

                    <div class="breadcrumbs">
                        <For each={breadcrumbs()}>
                            {(part, index) => (
                                <>
                                    <span>{part}</span>
                                    <Show
                                        when={
                                            index() < breadcrumbs().length - 1
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
                    </div>

                    <div
                        id="editor-surface"
                        class="editor-body"
                        ref={editorBody!}
                    >
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
                                    style={{
                                        top: `${Math.min(58, (view().cursor.line - view().firstLine + 1) * LINE_HEIGHT + 6)}px`,
                                        left: `${Math.min(70, (view().cursor.displayColumn - view().horizontalOffset) * cellWidth + 76)}px`,
                                    }}
                                >
                                    <For each={menu().items}>
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
                                <div class="hover-popover">
                                    <div class="popover-label">
                                        Documentation
                                    </div>
                                    <pre>{hover().content}</pre>
                                </div>
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
                                            <For each={picker().items}>
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
                                <span class="message">
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
                            <span class="connecting">connecting…</span>
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
                            <span class="git-counts">
                                <b>+{view().gitChanges.added}</b>
                                <i>~{view().gitChanges.modified}</i>
                                <em>−{view().gitChanges.removed}</em>
                            </span>
                            <span
                                classList={{
                                    has: view().diagnostics.errors > 0,
                                }}
                                class="problems"
                            >
                                <Icon name="status-error" tone="error" />
                                {view().diagnostics.errors}
                                <Icon name="status-warning" tone="warning" />
                                {view().diagnostics.warnings}
                            </span>
                        </div>
                        <div class="status-right">
                            <span>{view().language}</span>
                            <span>{view().encoding}</span>
                            <span>{view().lineEnding}</span>
                            <span>
                                {view().cursor.line + 1}:
                                {view().cursor.column + 1}
                            </span>
                        </div>
                    </footer>
                </section>
            </section>
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
                        <button onClick={() => void props.send(keys)}>
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
