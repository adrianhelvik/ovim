import {
    For,
    Show,
    createEffect,
    createMemo,
    createSignal,
    type JSX,
} from "solid-js";
import { Icon } from "./Icon";
import type { IconName } from "./icons.generated";

export type ContextPanelId = "ai" | "tests" | "debug";

export interface ContextPanelDefinition {
    id: ContextPanelId;
    label: string;
    state: string;
    icon: IconName;
    render: () => JSX.Element;
}

export default function ContextDock(props: {
    panels: ContextPanelDefinition[];
    activePanel?: ContextPanelId;
    onActivePanel?: (id: ContextPanelId) => void;
}) {
    const [activeId, setActiveId] = createSignal<ContextPanelId>(
        props.panels[0]?.id ?? "ai",
    );
    const activePanel = createMemo(
        () =>
            props.panels.find((panel) => panel.id === activeId()) ??
            props.panels[0],
    );

    createEffect(() => {
        const requested = props.activePanel;
        if (
            requested &&
            requested !== activeId() &&
            props.panels.some((panel) => panel.id === requested)
        ) {
            setActiveId(requested);
            return;
        }
        const panel = activePanel();
        if (!panel) return;
        if (panel.id !== activeId()) setActiveId(panel.id);
        props.onActivePanel?.(panel.id);
    });

    const selectPanel = (id: ContextPanelId) => {
        setActiveId(id);
        props.onActivePanel?.(id);
    };

    const moveFocus = (event: KeyboardEvent, panel: ContextPanelDefinition) => {
        const panels = props.panels;
        const current = panels.findIndex(
            (candidate) => candidate.id === panel.id,
        );
        let next = current;
        if (event.key === "ArrowRight") next = (current + 1) % panels.length;
        else if (event.key === "ArrowLeft")
            next = (current - 1 + panels.length) % panels.length;
        else if (event.key === "Home") next = 0;
        else if (event.key === "End") next = panels.length - 1;
        else return;

        event.preventDefault();
        const id = panels[next].id;
        selectPanel(id);
        queueMicrotask(() =>
            document.getElementById(`context-tab-${id}`)?.focus(),
        );
    };

    return (
        <Show when={activePanel()} keyed>
            {(active) => (
                <aside class="side-dock" aria-label="Context">
                    <Show when={props.panels.length > 1}>
                        <div
                            class="context-tabs"
                            role="tablist"
                            aria-label="Context panels"
                        >
                            <For each={props.panels}>
                                {(panel) => (
                                    <button
                                        id={`context-tab-${panel.id}`}
                                        type="button"
                                        role="tab"
                                        aria-label={panel.label}
                                        aria-selected={active.id === panel.id}
                                        aria-controls={`context-panel-${panel.id}`}
                                        tabIndex={
                                            active.id === panel.id ? 0 : -1
                                        }
                                        onClick={() => selectPanel(panel.id)}
                                        onKeyDown={(event) =>
                                            moveFocus(event, panel)
                                        }
                                    >
                                        <Icon name={panel.icon} size={16} />
                                        <span>{panel.label}</span>
                                        <small>{panel.state}</small>
                                    </button>
                                )}
                            </For>
                        </div>
                    </Show>
                    <div
                        id={`context-panel-${active.id}`}
                        class="context-panel"
                        role="tabpanel"
                        aria-label={
                            props.panels.length === 1 ? active.label : undefined
                        }
                        aria-labelledby={
                            props.panels.length > 1
                                ? `context-tab-${active.id}`
                                : undefined
                        }
                    >
                        {active.render()}
                    </div>
                </aside>
            )}
        </Show>
    );
}
