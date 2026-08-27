import { For, Show, type Accessor } from "solid-js";
import { browserTabTitle } from "./BrowserPanel";
import type { BrowserState } from "./browserProtocol";
import { Icon } from "./Icon";
import type { GuiSnapshot } from "./types";
import type { WorkbenchSelection, WorkbenchTabReference } from "./workbench";

interface WorkbenchTabStripProps {
    native: boolean;
    sourceTabs: GuiSnapshot["tabs"];
    tabs: WorkbenchTabReference[];
    selection: WorkbenchSelection;
    browserState: BrowserState;
    browserOpening: boolean;
    canRestoreBrowser: boolean;
    onSelect: (position: number) => void;
    onSourceFocus: () => void;
    onNewBrowser: () => void;
    onRestoreBrowser: () => void;
    onNavigate: (event: KeyboardEvent, position: number) => void;
}

export default function WorkbenchTabStrip(props: WorkbenchTabStripProps) {
    const selected = (tab: WorkbenchTabReference) => {
        const selection = props.selection;
        if (tab.kind !== selection.kind) return false;
        switch (tab.kind) {
            case "source":
                return (
                    selection.kind === "source" && tab.tabId === selection.tabId
                );
            case "vector":
                return (
                    selection.kind === "vector" &&
                    tab.sourceTabId === selection.sourceTabId
                );
            case "browser":
                return (
                    selection.kind === "browser" &&
                    tab.sessionId === selection.sessionId
                );
        }
    };

    const renderTab = (
        reference: WorkbenchTabReference,
        position: Accessor<number>,
    ) => {
        const active = () => selected(reference);
        const common = {
            role: "tab",
            "aria-controls": "editor-surface",
        } as const;

        switch (reference.kind) {
            case "source": {
                const source = () =>
                    props.sourceTabs.find((tab) => tab.id === reference.tabId);
                return (
                    <Show when={source()}>
                        {(tab) => (
                            <button
                                {...common}
                                type="button"
                                aria-selected={active()}
                                tabIndex={active() ? 0 : -1}
                                data-tab-index={tab().index}
                                data-workbench-tab-index={position()}
                                class="tab"
                                classList={{ active: active() }}
                                aria-label={
                                    tab().title +
                                    (tab().modified ? ", modified" : "")
                                }
                                title={
                                    tab().title +
                                    (tab().modified ? " · modified" : "")
                                }
                                onClick={() => {
                                    props.onSelect(position());
                                    props.onSourceFocus();
                                }}
                                onKeyDown={(event) =>
                                    props.onNavigate(event, position())
                                }
                            >
                                <Icon
                                    name="file"
                                    size={16}
                                    tone={active() ? "accent" : "muted"}
                                />
                                <span>{tab().title}</span>
                                <Show when={tab().modified}>
                                    <span class="modified-dot" />
                                </Show>
                            </button>
                        )}
                    </Show>
                );
            }
            case "vector":
                return (
                    <button
                        {...common}
                        type="button"
                        aria-selected={active()}
                        tabIndex={active() ? 0 : -1}
                        data-workbench-tab-index={position()}
                        class="tab vector-tab"
                        classList={{ active: active() }}
                        title="Live Strøk render and review"
                        onClick={() => props.onSelect(position())}
                        onKeyDown={(event) =>
                            props.onNavigate(event, position())
                        }
                    >
                        <Icon
                            name="ai-spark"
                            size={16}
                            tone={active() ? "accent" : "muted"}
                        />
                        <span>Vector</span>
                    </button>
                );
            case "browser": {
                const session = () =>
                    props.browserState.sessions.find(
                        (candidate) =>
                            candidate.sessionId === reference.sessionId,
                    );
                const title = () =>
                    session() ? browserTabTitle(session()!) : "Browser";
                return (
                    <Show when={session()}>
                        {(current) => (
                            <button
                                {...common}
                                type="button"
                                aria-selected={active()}
                                aria-label={`Browser: ${title()}`}
                                tabIndex={active() ? 0 : -1}
                                data-workbench-tab-index={position()}
                                class="tab browser-tab"
                                classList={{ active: active() }}
                                title={
                                    current().url
                                        ? `${title()} · ${current().url}`
                                        : title()
                                }
                                onClick={() => props.onSelect(position())}
                                onKeyDown={(event) =>
                                    props.onNavigate(event, position())
                                }
                            >
                                <Icon
                                    name="search"
                                    size={16}
                                    tone={active() ? "accent" : "muted"}
                                />
                                <span>{title()}</span>
                                <Show when={current().loading}>
                                    <span
                                        class="browser-tab-loading"
                                        aria-label="Loading"
                                    />
                                </Show>
                            </button>
                        )}
                    </Show>
                );
            }
        }
    };

    return (
        <div class="tabs" role="tablist" aria-label="Open tabs">
            <For each={props.tabs}>{renderTab}</For>
            <Show when={props.canRestoreBrowser}>
                <button
                    type="button"
                    class="restore-browser-tab"
                    data-gui-native-control
                    disabled={
                        !props.native ||
                        props.browserOpening ||
                        props.browserState.sessions.length >=
                            props.browserState.maxSessions
                    }
                    aria-label="Restore closed browser tab"
                    title={
                        props.browserState.sessions.length >=
                        props.browserState.maxSessions
                            ? `Close a Browser tab before restoring one (limit ${props.browserState.maxSessions})`
                            : "Restore closed Browser tab · X or Cmd/Ctrl+Shift+T in Browser"
                    }
                    onClick={props.onRestoreBrowser}
                >
                    <Icon name="restore" size={16} />
                </button>
            </Show>
            <button
                type="button"
                class="new-browser-tab"
                data-gui-native-control
                disabled={
                    !props.native ||
                    props.browserOpening ||
                    props.browserState.sessions.length >=
                        props.browserState.maxSessions
                }
                aria-label="New browser tab"
                title={
                    props.native
                        ? props.browserState.sessions.length >=
                          props.browserState.maxSessions
                            ? `Browser tab limit (${props.browserState.maxSessions}) reached`
                            : "New browser tab"
                        : "Browser tabs require the native desktop app"
                }
                onClick={props.onNewBrowser}
            >
                <span aria-hidden="true">+</span>
            </button>
            <span class="tabs-fill" role="presentation" />
        </div>
    );
}
