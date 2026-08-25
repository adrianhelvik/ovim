import { Show, createEffect, createSignal, onCleanup, onMount } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Icon } from "./Icon";

export interface BrowserSession {
    sessionId: string;
    url: string;
    title: string;
    visible: boolean;
    loading: boolean;
    documentId: number;
}

interface BrowserState {
    session?: BrowserSession;
}

interface BrowserPanelProps {
    native: boolean;
    active: boolean;
    obscured: boolean;
    onClose: () => void;
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

export default function BrowserPanel(props: BrowserPanelProps) {
    const [state, setState] = createSignal<BrowserState>({});
    const [address, setAddress] = createSignal("");
    const [error, setError] = createSignal("");
    const [opening, setOpening] = createSignal(false);
    let viewport: HTMLDivElement | undefined;
    let addressInput: HTMLInputElement | undefined;
    let boundsFrame: number | undefined;

    const session = () => state().session;
    const browserVisible = () =>
        props.native &&
        props.active &&
        !props.obscured &&
        document.visibilityState !== "hidden";

    const projectState = (next: BrowserState) => {
        setState(next);
        const url = next.session?.url ?? "";
        if (document.activeElement !== addressInput) setAddress(url);
    };

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

    const open = async () => {
        if (!props.native || !props.active || opening()) return;
        setOpening(true);
        setError("");
        try {
            projectState(await invoke<BrowserState>("gui_browser_open"));
            scheduleBounds();
        } catch (reason) {
            setError(String(reason));
        } finally {
            setOpening(false);
        }
    };

    const runToolbarAction = async (action: "back" | "forward" | "reload") => {
        setError("");
        try {
            await invoke("gui_browser_toolbar", { action });
        } catch (reason) {
            setError(String(reason));
        }
    };

    const navigate = async () => {
        const url = normalizeAddress(address());
        if (!url) return;
        setAddress(url);
        setError("");
        try {
            projectState(
                await invoke<BrowserState>("gui_browser_navigate", { url }),
            );
        } catch (reason) {
            setError(String(reason));
        }
    };

    const close = async () => {
        if (!props.native || !session()) {
            props.onClose();
            return;
        }
        setError("");
        try {
            await invoke("gui_browser_close");
            projectState({});
            props.onClose();
        } catch (reason) {
            setError(String(reason));
        }
    };

    createEffect(() => {
        if (!props.active) {
            sendBounds(false);
            return;
        }
        if (props.native) queueMicrotask(() => void open());
    });

    createEffect(() => {
        void props.active;
        void props.obscured;
        scheduleBounds();
    });

    onMount(() => {
        let disposed = false;
        let unlistenState: (() => void) | undefined;
        if (props.native) {
            if (!props.active) {
                void invoke<BrowserState>("gui_browser_state")
                    .then(projectState)
                    .catch((reason) => setError(String(reason)));
            }
            void listen<BrowserState>("ovim://browser-state", (event) =>
                projectState(event.payload),
            ).then((unlisten) => {
                if (disposed) unlisten();
                else unlistenState = unlisten;
            });
        }

        const observer = new ResizeObserver(scheduleBounds);
        if (viewport) observer.observe(viewport);
        window.addEventListener("resize", scheduleBounds);
        window.addEventListener("scroll", scheduleBounds, true);
        document.addEventListener("visibilitychange", scheduleBounds);

        onCleanup(() => {
            disposed = true;
            unlistenState?.();
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
                        disabled={!session()}
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
                        disabled={!session()}
                        aria-label="Go forward"
                        title="Go forward"
                        onClick={() => void runToolbarAction("forward")}
                    >
                        <Icon name="chevron-right" size={16} />
                    </button>
                    <button
                        type="button"
                        data-gui-native-control
                        disabled={!session()}
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
                        : opening()
                          ? "Starting an isolated browser session…"
                          : "No browser session is open"}
                </span>
                <Show when={error()}>
                    <em role="alert">{error()}</em>
                </Show>
            </div>

            <div ref={viewport} class="browser-viewport">
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
                <Show when={props.native && !session() && !opening()}>
                    <button
                        type="button"
                        class="browser-open-action"
                        data-gui-native-control
                        onClick={() => void open()}
                    >
                        Open browser session
                    </button>
                </Show>
            </div>
        </section>
    );
}
