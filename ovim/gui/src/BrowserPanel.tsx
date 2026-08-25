import { Show, createEffect, createSignal, onCleanup, onMount } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { Icon } from "./Icon";
import { normalizeBrowserAddress } from "./browserCommands";
import type { BrowserToolbarAction } from "./browserWorkbench";

export interface BrowserSession {
    sessionId: string;
    url: string;
    title: string;
    visible: boolean;
    loading: boolean;
    documentId: number;
    vimKeysEnabled: boolean;
    keyMode: "normal" | "insert";
}

export interface BrowserState {
    sessions: BrowserSession[];
    activeSessionId?: string;
    maxSessions: number;
    presentationRequest?: {
        revision: number;
        sessionId: string;
    };
}

interface BrowserPanelProps {
    native: boolean;
    active: boolean;
    obscured: boolean;
    session?: BrowserSession;
    addressFocusRequest?: { serial: number; sessionId: string };
    onNavigate: (sessionId: string, url: string) => Promise<void>;
    onToolbar: (
        sessionId: string,
        action: BrowserToolbarAction,
    ) => Promise<void>;
    onClose: (sessionId: string) => Promise<void>;
    onVimKeysChange: (sessionId: string, enabled: boolean) => Promise<void>;
}

interface BrowserBounds {
    x: number;
    y: number;
    width: number;
    height: number;
    visible: boolean;
}

export const browserTabTitle = (session: BrowserSession) => {
    const title = session.title.trim();
    if (title) return title;
    try {
        return new URL(session.url).hostname || "Browser";
    } catch {
        return "Browser";
    }
};

export default function BrowserPanel(props: BrowserPanelProps) {
    const [address, setAddress] = createSignal("");
    const [error, setError] = createSignal("");
    let viewport: HTMLDivElement | undefined;
    let addressInput: HTMLInputElement | undefined;
    let boundsFrame: number | undefined;
    let projectedSessionId = "";
    let handledAddressFocusSerial = 0;

    const session = () => props.session;
    const browserVisible = () =>
        props.native &&
        props.active &&
        Boolean(session()) &&
        !props.obscured &&
        document.visibilityState !== "hidden";

    createEffect(() => {
        const request = props.addressFocusRequest;
        if (
            !request ||
            request.serial <= handledAddressFocusSerial ||
            request.sessionId !== session()?.sessionId
        )
            return;
        handledAddressFocusSerial = request.serial;
        queueMicrotask(() => {
            addressInput?.focus({ preventScroll: true });
            addressInput?.select();
        });
    });

    const sendBounds = (visible = browserVisible()) => {
        if (!props.native) return;
        const rect = viewport?.getBoundingClientRect();
        const bounds: BrowserBounds = {
            x: rect?.left ?? 0,
            y: rect?.top ?? 0,
            width: rect?.width ?? 0,
            height: rect?.height ?? 0,
            visible:
                visible && Boolean(rect && rect.width >= 1 && rect.height >= 1),
        };
        void invoke("gui_browser_set_bounds", { bounds }).catch((reason) => {
            if (props.active) setError(String(reason));
        });
    };

    const scheduleBounds = () => {
        if (boundsFrame !== undefined) cancelAnimationFrame(boundsFrame);
        boundsFrame = requestAnimationFrame(() => {
            boundsFrame = undefined;
            sendBounds();
        });
    };

    const runToolbarAction = async (action: "back" | "forward" | "reload") => {
        const sessionId = session()?.sessionId;
        if (!sessionId) return;
        setError("");
        try {
            await props.onToolbar(sessionId, action);
        } catch (reason) {
            setError(String(reason));
        }
    };

    const navigate = async () => {
        const sessionId = session()?.sessionId;
        const url = normalizeBrowserAddress(address());
        if (!sessionId || !url) return;
        setAddress(url);
        setError("");
        try {
            await props.onNavigate(sessionId, url);
        } catch (reason) {
            setError(String(reason));
        }
    };

    const close = async () => {
        const sessionId = session()?.sessionId;
        if (!props.native || !sessionId) return;
        setError("");
        try {
            await props.onClose(sessionId);
        } catch (reason) {
            setError(String(reason));
        }
    };

    const toggleVimKeys = async () => {
        const current = session();
        if (!props.native || !current) return;
        setError("");
        try {
            await props.onVimKeysChange(
                current.sessionId,
                !current.vimKeysEnabled,
            );
        } catch (reason) {
            setError(String(reason));
        }
    };

    createEffect(() => {
        const next = session();
        if (projectedSessionId !== (next?.sessionId ?? "")) {
            projectedSessionId = next?.sessionId ?? "";
            setError("");
            if (props.active && next && !next.url)
                queueMicrotask(() => addressInput?.focus());
        }
        if (document.activeElement !== addressInput)
            setAddress(next?.url ?? "");
    });

    createEffect(() => {
        void props.active;
        void props.obscured;
        void session()?.sessionId;
        scheduleBounds();
    });

    onMount(() => {
        const observer = new ResizeObserver(scheduleBounds);
        if (viewport) observer.observe(viewport);
        window.addEventListener("resize", scheduleBounds);
        window.addEventListener("scroll", scheduleBounds, true);
        document.addEventListener("visibilitychange", scheduleBounds);

        onCleanup(() => {
            observer.disconnect();
            window.removeEventListener("resize", scheduleBounds);
            window.removeEventListener("scroll", scheduleBounds, true);
            document.removeEventListener("visibilitychange", scheduleBounds);
            if (boundsFrame !== undefined) cancelAnimationFrame(boundsFrame);
            sendBounds(false);
        });
    });

    return (
        <section
            class="browser-workbench"
            classList={{ active: props.active }}
            aria-label="Embedded browser"
            aria-hidden={!props.active}
        >
            <header class="browser-toolbar">
                <div class="browser-history" aria-label="Browser history">
                    <button
                        type="button"
                        data-gui-native-control
                        disabled={!session()?.url}
                        aria-label="Go back"
                        title="Go back"
                        onClick={() => void runToolbarAction("back")}
                    >
                        <span class="browser-back-icon">
                            <Icon name="chevron-right" size={16} />
                        </span>
                    </button>
                    <button
                        type="button"
                        data-gui-native-control
                        disabled={!session()?.url}
                        aria-label="Go forward"
                        title="Go forward"
                        onClick={() => void runToolbarAction("forward")}
                    >
                        <Icon name="chevron-right" size={16} />
                    </button>
                    <button
                        type="button"
                        data-gui-native-control
                        disabled={!session()?.url}
                        aria-label="Reload page"
                        onClick={() => void runToolbarAction("reload")}
                    >
                        Reload
                    </button>
                </div>

                <form
                    class="browser-address"
                    onSubmit={(event) => {
                        event.preventDefault();
                        void navigate();
                    }}
                >
                    <span
                        classList={{
                            loading: Boolean(session()?.loading),
                            ready: Boolean(session() && !session()?.loading),
                        }}
                        aria-hidden="true"
                    />
                    <input
                        ref={addressInput}
                        data-gui-native-control
                        type="text"
                        inputmode="url"
                        spellcheck={false}
                        autocomplete="off"
                        aria-label="Browser address"
                        placeholder="Enter an HTTP or HTTPS address"
                        value={address()}
                        onInput={(event) =>
                            setAddress(event.currentTarget.value)
                        }
                        onBlur={() => setAddress(session()?.url ?? address())}
                    />
                </form>

                <div class="browser-session-controls">
                    <Show when={session()}>
                        {(current) => (
                            <button
                                type="button"
                                class="browser-key-toggle"
                                data-gui-native-control
                                aria-pressed={current().vimKeysEnabled}
                                aria-label={
                                    current().vimKeysEnabled
                                        ? "Disable Vim-style page keys"
                                        : "Enable Vim-style page keys"
                                }
                                title={
                                    current().vimKeysEnabled
                                        ? "Vim-style page keys are on · i enters Insert mode · ? shows help"
                                        : "Vim-style page keys are off · browser shortcuts still work"
                                }
                                onClick={() => void toggleVimKeys()}
                            >
                                <Icon name="command" size={16} />
                                <span>
                                    {current().vimKeysEnabled
                                        ? current().keyMode === "insert"
                                            ? "Insert"
                                            : "Vim keys"
                                        : "Keys off"}
                                </span>
                            </button>
                        )}
                    </Show>
                    <Show when={session()}>
                        <span
                            class="browser-agent-state"
                            title="The AI agent and you can inspect the same browser session"
                        >
                            <Icon name="agent" size={16} tone="accent" />
                            shared session
                        </span>
                    </Show>
                    <button
                        type="button"
                        data-gui-native-control
                        disabled={!session()}
                        aria-label="Close browser session"
                        title="Close browser session"
                        onClick={() => void close()}
                    >
                        <Icon name="close" size={16} />
                    </button>
                </div>
            </header>

            <div class="browser-page-meta">
                <strong>{session()?.title || "Embedded browser"}</strong>
                <span>
                    {session()
                        ? "Agent actions use fresh, bounded page snapshots"
                        : "No browser session is selected"}
                </span>
                <Show when={error()}>
                    <em role="alert">{error()}</em>
                </Show>
            </div>

            <div ref={viewport} class="browser-viewport">
                <Show when={props.native && session() && !session()?.url}>
                    <div class="browser-empty-state">
                        <Icon name="command" size={24} tone="accent" />
                        <b>Where do you want to go?</b>
                        <span>
                            Enter an address above. The browser is created only
                            when you navigate.
                        </span>
                    </div>
                </Show>
                <Show when={!props.native}>
                    <div class="browser-empty-state">
                        <Icon name="command" size={24} tone="accent" />
                        <b>The embedded browser runs in the Ovim desktop app</b>
                        <span>
                            This web preview cannot create a native child
                            webview.
                        </span>
                    </div>
                </Show>
            </div>
        </section>
    );
}
