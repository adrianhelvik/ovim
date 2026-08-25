import { For, Index, Show } from "solid-js";
import { browserTabTitle, type BrowserState } from "./BrowserPanel";
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
    onSelect: (position: number) => void;
    onSourceFocus: () => void;
    onNewBrowser: () => void;
    onNavigate: (event: KeyboardEvent, position: number) => void;
}

export default function WorkbenchTabStrip(props: WorkbenchTabStripProps) {
    const positionOf = (kind: WorkbenchTabReference["kind"], id?: string) =>
        props.tabs.findIndex(
            (tab) =>
                tab.kind === kind &&
                (tab.kind !== "browser" || tab.sessionId === id),
        );
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

    return (
        <div class="tabs" role="tablist" aria-label="Open tabs">
            <Index each={props.sourceTabs}>
                {(tab, position) => {
                    const reference = () => props.tabs[position];
                    const active = () =>
                        Boolean(reference() && selected(reference()!));
                    return (
                        <button
                            type="button"
                            role="tab"
                            aria-selected={active()}
                            aria-controls="editor-surface"
                            tabIndex={active() ? 0 : -1}
                            data-tab-index={tab().index}
                            data-workbench-tab-index={position}
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
                                props.onSelect(position);
                                props.onSourceFocus();
                            }}
                            onKeyDown={(event) =>
                                props.onNavigate(event, position)
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
                    );
                }}
            </Index>
            <Show when={positionOf("vector") >= 0}>
                {(() => {
                    const position = () => positionOf("vector");
                    const reference = () => props.tabs[position()];
                    const active = () =>
                        Boolean(reference() && selected(reference()!));
                    return (
                        <button
                            type="button"
                            role="tab"
                            aria-selected={active()}
                            aria-controls="editor-surface"
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
                })()}
            </Show>
            <For each={props.browserState.sessions}>
                {(session) => {
                    const position = () =>
                        positionOf("browser", session.sessionId);
                    const reference = () => props.tabs[position()];
                    const active = () =>
                        Boolean(reference() && selected(reference()!));
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
                            title={
                                session.url
                                    ? `${title()} · ${session.url}`
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
