import { Show, createEffect, createSignal, onCleanup, onMount } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { Icon } from "./Icon";

export interface BrowserSession {
    sessionId: string;
    url: string;
    title: string;
    visible: boolean;
    loading: boolean;
    documentId: number;
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
    onState: (state: BrowserState) => void;
}

interface BrowserBounds {
    x: number;
    y: number;
    width: number;
    height: number;
    visible: boolean;
}

const normalizeAddress = (value: string) => {
    const address = value.trim();
    if (!address || /^[a-z][a-z\d+.-]*:/i.test(address)) return address;
    return `https://${address}`;
};

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

    const session = () => props.session;
    const browserVisible = () =>
        props.native &&
        props.active &&
        Boolean(session()) &&
        !props.obscured &&
        document.visibilityState !== "hidden";

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
            await invoke("gui_browser_toolbar", { sessionId, action });
        } catch (reason) {
            setError(String(reason));
        }
    };

    const navigate = async () => {
        const sessionId = session()?.sessionId;
        const url = normalizeAddress(address());
        if (!sessionId || !url) return;
        setAddress(url);
        setError("");
        try {
            props.onState(
                await invoke<BrowserState>("gui_browser_navigate", {
                    sessionId,
                    url,
                }),
            );
        } catch (reason) {
            setError(String(reason));
        }
    };

    const close = async () => {
        const sessionId = session()?.sessionId;
        if (!props.native || !sessionId) return;
        setError("");
        try {
            props.onState(
                await invoke<BrowserState>("gui_browser_close", { sessionId }),
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
