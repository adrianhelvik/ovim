import {
    Show,
    createEffect,
    createMemo,
    createSignal,
    onCleanup,
    onMount,
} from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { Icon } from "./Icon";
import { normalizeBrowserAddress } from "./browserCommands";
import type {
    BrowserAddressFocusRequest,
    BrowserBounds,
    BrowserSession,
    BrowserToolbarAction,
} from "./browserProtocol";

export type { BrowserSession, BrowserState } from "./browserProtocol";

interface BrowserPanelProps {
    native: boolean;
    active: boolean;
    obscured: boolean;
    session?: BrowserSession;
    addressFocusRequest?: BrowserAddressFocusRequest;
    onNavigate: (sessionId: string, url: string) => Promise<void>;
    onToolbar: (
        sessionId: string,
        action: BrowserToolbarAction,
    ) => Promise<void>;
    onClose: (sessionId: string) => Promise<void>;
    onVimKeysChange: (sessionId: string, enabled: boolean) => Promise<void>;
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
    const sessionId = createMemo(() => session()?.sessionId ?? "");
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

    const runToolbarAction = async (
        action: "back" | "forward" | "reload" | "stop",
    ) => {
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
            queueMicrotask(() => {
                addressInput?.focus({ preventScroll: true });
                addressInput?.select();
            });
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
        void sessionId();
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
                        aria-keyshortcuts="Meta+[ Control+["
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
                        aria-keyshortcuts="Meta+] Control+]"
                        title="Go forward"
                        onClick={() => void runToolbarAction("forward")}
                    >
                        <Icon name="chevron-right" size={16} />
                    </button>
                    <button
                        type="button"
                        data-gui-native-control
                        disabled={!session()?.url}
                        aria-label={
                            session()?.loading ? "Stop loading" : "Reload page"
                        }
                        aria-keyshortcuts="Meta+R Control+R"
                        title={
                            session()?.loading ? "Stop loading" : "Reload page"
                        }
                        onClick={() =>
                            void runToolbarAction(
                                session()?.loading ? "stop" : "reload",
                            )
                        }
                    >
                        {session()?.loading ? "Stop" : "Reload"}
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
                        aria-keyshortcuts="Meta+L Control+L"
                        placeholder="Search or enter an address"
                        value={address()}
                        onInput={(event) =>
                            setAddress(event.currentTarget.value)
                        }
                        onBlur={() => setAddress(session()?.url ?? address())}
                        onKeyDown={(event) => {
                            const sessionId = session()?.sessionId;
                            if (event.key !== "Escape" || !sessionId) return;
                            event.preventDefault();
                            setAddress(session()?.url ?? "");
                            void props
                                .onToolbar(sessionId, "focus")
                                .catch((reason) => setError(String(reason)));
                        }}
                    />
                </form>

                <div class="browser-session-controls">
                    <Show when={session()}>
                        {(current) => (
                            <>
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
                                            ? "Vim-style page keys are on · fields enter Insert mode · ? shows help"
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
                                <span
                                    class="browser-agent-state"
                                    title="The AI agent and you can inspect the same browser session"
                                >
                                    <Icon
                                        name="agent"
                                        size={16}
                                        tone="accent"
                                    />
                                    shared session
                                </span>
                            </>
                        )}
                    </Show>
                    <button
                        type="button"
                        data-gui-native-control
                        disabled={!session()}
                        aria-label="Close browser session"
                        aria-keyshortcuts="Meta+W Control+W"
                        title="Close browser session"
                        onClick={() => void close()}
                    >
                        <Icon name="close" size={16} />
                    </button>
                </div>
            </header>

            <div
                class="browser-notice"
                classList={{ visible: Boolean(error()) }}
                aria-live="polite"
            >
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
                            Search or enter an address above. The browser is
                            created only when you navigate.
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
