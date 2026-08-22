import { For, Show, createEffect, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import { Channel, invoke, isTauri } from "@tauri-apps/api/core";
import DOMPurify from "dompurify";
import { marked } from "marked";
import { mockSnapshot } from "./mock";
import type { GuiAiChat, GuiKeyInput, GuiLayoutNode, GuiPane, GuiSnapshot } from "./types";

const LINE_HEIGHT = 22;
const FALLBACK_CELL_WIDTH = 8.15;

export const Markdown = (props: { text: string }) => {
  const html = createMemo(() => DOMPurify.sanitize(
    marked.parse(props.text, { async: false, breaks: true, gfm: true }) as string,
    { USE_PROFILES: { html: true } },
  ));
  return <div class="markdown" innerHTML={html()} />;
};

export const isNearChatBottom = (element: Pick<HTMLElement, "scrollHeight" | "scrollTop" | "clientHeight">) =>
  element.scrollHeight - element.scrollTop - element.clientHeight <= 48;

export const ChatPanel = (props: { chat: GuiAiChat; focusInput: () => void }) => {
  const [following, setFollowing] = createSignal(true);
  let transcript!: HTMLDivElement;
  let messageCount = props.chat.messages.length;

  const jumpToLatest = () => {
    transcript.scrollTop = transcript.scrollHeight;
    setFollowing(true);
  };

  createEffect(() => {
    const messages = props.chat.messages;
    const latest = messages.at(-1);
    const revision = `${messages.length}:${latest?.content.length ?? 0}:${props.chat.streaming?.length ?? 0}:${props.chat.approval?.length ?? 0}`;
    if (messages.length > messageCount && latest?.role === "user") setFollowing(true);
    messageCount = messages.length;
    void revision;
    queueMicrotask(() => {
      if (following()) jumpToLatest();
    });
  });

  return (
    <section class="side-panel ai-panel" aria-label="AI chat">
      <header class="side-panel-header">
        <div><b>AI chat</b><small>{props.chat.profile} · {props.chat.reasoningEffort}</small></div>
        <span classList={{ working: props.chat.activity !== "idle" }}>{props.chat.activity.replaceAll("_", " ")}</span>
      </header>
      <div class="chat-transcript">
        <div
          class="chat-messages"
          ref={transcript}
          onScroll={() => setFollowing(isNearChatBottom(transcript))}
        >
          <For each={props.chat.messages}>{(message) => (
            <article class={`chat-message ${message.role}`}>
              <header><b>{message.role}</b><small>{message.model}</small></header>
              <Markdown text={message.content} />
              <Show when={message.tools.length}><div class="tool-chips"><For each={message.tools}>{(tool) => <span>{tool}</span>}</For></div></Show>
            </article>
          )}</For>
          <Show when={props.chat.streaming}>{(content) => <article class="chat-message assistant streaming"><header><b>assistant</b><small>streaming</small></header><Markdown text={content()} /></article>}</Show>
        </div>
        <Show when={!following()}><button class="chat-jump" onClick={() => { jumpToLatest(); props.focusInput(); }}>↓ Jump to latest</button></Show>
      </div>
      <Show when={props.chat.approval}>{(approval) => <div class="approval-card"><b>Approval required</b><span>{approval()}</span><small>Use the keyboard choices shown by Ovim.</small></div>}</Show>
      <div onMouseDown={props.focusInput}><ChatComposer chat={props.chat} /></div>
    </section>
  );
};

const splitAtUtf8Offset = (text: string, offset: number) => {
  const limit = Math.max(0, Math.min(offset, new TextEncoder().encode(text).length));
  let bytes = 0;
  let codeUnits = 0;
  for (const character of text) {
    const next = bytes + new TextEncoder().encode(character).length;
    if (next > limit) break;
    bytes = next;
    codeUnits += character.length;
  }
  return [text.slice(0, codeUnits), text.slice(codeUnits)] as const;
};

export const ChatComposer = (props: { chat: GuiAiChat }) => {
  const parts = createMemo(() => splitAtUtf8Offset(props.chat.input, props.chat.inputCursor));
  return (
    <div class="chat-composer" classList={{ waiting: props.chat.waiting }}>
      <Show when={props.chat.pendingImages.length}>
        <div class="chat-attachments" aria-label="Pending image attachments">
          <For each={props.chat.pendingImages}>{(name) => <span title={name}>▧ {name}</span>}</For>
        </div>
      </Show>
      <pre aria-label="AI chat input"><Show when={props.chat.input} fallback={<><i class="chat-caret" aria-hidden="true" /><span class="chat-placeholder">Ask Ovim about this code…</span></>}><span>{parts()[0]}</span><i class="chat-caret" aria-hidden="true" /><span>{parts()[1]}</span></Show></pre>
      <footer><span>{props.chat.waiting ? "working" : "Enter to send · drop images to attach · Esc to return"}</span><b>{props.chat.reasoningEffort}</b></footer>
    </div>
  );
};

const Icon = (props: { name: "files" | "search" | "branch" | "spark" | "gear" | "close" | "min" | "max" }) => {
  const paths: Record<string, string> = {
    files: "M4 3.5h6l2 2H20v15H4z M8 1.5h6l2 2",
    search: "m20 20-4.5-4.5 M10.5 17a6.5 6.5 0 1 1 0-13 6.5 6.5 0 0 1 0 13",
    branch: "M6 3v12a4 4 0 0 0 4 4h5 M6 7h7a4 4 0 0 0 4-4v12 M3.5 3A2.5 2.5 0 1 0 8.5 3 2.5 2.5 0 0 0 3.5 3 M14.5 19a2.5 2.5 0 1 0 5 0 2.5 2.5 0 0 0-5 0",
    spark: "m12 2 1.8 6.2L20 10l-6.2 1.8L12 18l-1.8-6.2L4 10l6.2-1.8z",
    gear: "M12 8.5a3.5 3.5 0 1 0 0 7 3.5 3.5 0 0 0 0-7 M19 13.5v-3l-2.1-.7-.7-1.6 1-2-2.1-2.1-2 1-1.6-.7L10.5 2h-3l-.7 2.1-1.6.7-2-1L1.1 5.9l1 2-.7 1.6-2.1.7v3l2.1.7.7 1.6-1 2 2.1 2.1 2-1 1.6.7.7 2.1h3l.7-2.1 1.6-.7 2 1 2.1-2.1-1-2 .7-1.6z",
    close: "m7 7 10 10M17 7 7 17",
    min: "M6 12h12",
    max: "M7 7h10v10H7z",
  };
  return <svg viewBox="0 0 24 24" aria-hidden="true"><path d={paths[props.name]} /></svg>;
};

function App() {
  const native = isTauri();
  const [view, setView] = createSignal<GuiSnapshot>(mockSnapshot);
  const [error, setError] = createSignal("");
  const [connected, setConnected] = createSignal(!native);
  const [composition, setComposition] = createSignal("");
  let editorBody!: HTMLDivElement;
  let inputSink!: HTMLTextAreaElement;
  let cellWidth = FALLBACK_CELL_WIDTH;
  let composing = false;
  let ignoreNextInput = false;
  let wheelRemainder = 0;
  let lastDimensions = { columns: 0, rows: 0 };

  const dimensions = () => {
    const paneTree = editorBody?.querySelector<HTMLElement>(".pane-tree");
    const paneColumns = Math.floor((paneTree?.clientWidth || editorBody?.clientWidth || 960) / cellWidth);
    // The shared core viewport contract consumes full terminal dimensions and
    // subtracts its own tree/status/tab chrome. Add those cells back after
    // measuring the DOM's already-narrowed editor surface.
    const coreChrome = view().fileTree ? 50 : 0;
    return {
      columns: Math.max(20, paneColumns + coreChrome),
      rows: Math.max(5, Math.floor((editorBody?.clientHeight || 600) / LINE_HEIGHT) + 2 + (view().tabs.length > 1 ? 1 : 0)),
    };
  };

  const syncDimensions = () => {
    if (!native) return;
    const next = dimensions();
    if (next.columns === lastDimensions.columns && next.rows === lastDimensions.rows) return;
    lastDimensions = next;
    void invoke("gui_snapshot", next).catch((reason) => setError(String(reason)));
  };

  const accept = (snapshot: GuiSnapshot) => {
    setView(snapshot);
    setConnected(true);
    setError("");
    requestAnimationFrame(syncDimensions);
    if (snapshot.shouldQuit && native) void windowAction("close");
  };

  const mutate = async (command: string, args: Record<string, unknown>) => {
    if (!native) return;
    try {
      await invoke(command, args);
    } catch (reason) {
      setError(String(reason));
    }
  };

  const sendKey = (input: GuiKeyInput) => mutate("gui_key", { input });
  const sendLiteral = async (keys: string) => {
    for (const key of keys) {
      await sendKey({ key, shift: key.toUpperCase() === key && key.toLowerCase() !== key, control: false, alt: false, meta: false });
    }
  };
  const windowAction = (action: string) => invoke<void>("gui_window_action", { action });

  const themeVars = createMemo(() => {
    const theme = view().theme;
    return {
      "--bg": theme.background,
      "--fg": theme.foreground,
      "--surface": theme.surface,
      "--surface-selected": theme.surfaceSelected,
      "--border": theme.border,
      "--accent": theme.accent,
      "--accent-fg": theme.accentForeground,
      "--muted": theme.muted,
      "--cursor-line": theme.cursorLine,
      "--selection": theme.selection,
      "--search": theme.search,
      "--error": theme.error,
      "--warning": theme.warning,
      "--info": theme.info,
      "--success": theme.success,
      "--cell-width": `${cellWidth}px`,
    };
  });

  const breadcrumbs = createMemo(() => {
    const path = view().filePath;
    if (!path) return [view().fileName];
    return path.split(/[\\/]/).filter(Boolean).slice(-4);
  });

  const handleKeyDown = (event: KeyboardEvent) => {
    if (event.isComposing || event.key === "Process" || event.key === "Dead") return;
    const clipboardModifier = /Mac|iPhone|iPad/.test(navigator.platform)
      ? event.metaKey
      : event.ctrlKey && event.shiftKey;
    if (clipboardModifier && ["c", "v", "x"].includes(event.key.toLowerCase())) return;
    event.preventDefault();
    void sendKey({
      key: event.key,
      shift: event.shiftKey,
      control: event.ctrlKey,
      alt: event.altKey,
      meta: event.metaKey,
    });
  };

  const handlePaste = (event: ClipboardEvent) => {
    const text = event.clipboardData?.getData("text/plain");
    if (!text) return;
    event.preventDefault();
    void mutate("gui_paste", { text });
  };

  const handleCopy = (event: ClipboardEvent) => {
    const text = view().selectionText;
    if (!text) return;
    event.clipboardData?.setData("text/plain", text);
    event.preventDefault();
  };

  const handleCut = (event: ClipboardEvent) => {
    const text = view().selectionText;
    if (!text) return;
    event.clipboardData?.setData("text/plain", text);
    event.preventDefault();
    void sendKey({ key: "d", shift: false, control: false, alt: false, meta: false });
  };

  const handleCompositionStart = () => {
    composing = true;
    setComposition("");
  };

  const handleCompositionUpdate = (event: CompositionEvent) => setComposition(event.data);

  const handleCompositionEnd = (event: CompositionEvent) => {
    composing = false;
    setComposition("");
    ignoreNextInput = true;
    if (event.data) void mutate("gui_paste", { text: event.data });
    queueMicrotask(() => { inputSink.value = ""; });
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
    const pane = (event.target as Element | null)?.closest<HTMLElement>(".editor-pane");
    if (!pane) return;
    event.preventDefault();
    const scale = event.deltaMode === WheelEvent.DOM_DELTA_LINE ? LINE_HEIGHT : event.deltaMode === WheelEvent.DOM_DELTA_PAGE ? editorBody.clientHeight : 1;
    wheelRemainder += event.deltaY * scale;
    const count = Math.min(8, Math.floor(Math.abs(wheelRemainder) / LINE_HEIGHT));
    if (count === 0) return;
    const direction = Math.sign(wheelRemainder);
    wheelRemainder -= direction * count * LINE_HEIGHT;
    const paneIndex = Number(pane.dataset.pane);
    if (Number.isFinite(paneIndex) && !pane.classList.contains("focused")) {
      await mutate("gui_focus_pane", { index: paneIndex });
    }
    const key = direction > 0 ? "e" : "y";
    for (let index = 0; index < count; index += 1) {
      await sendKey({ key, shift: false, control: true, alt: false, meta: false });
    }
  };

  const setCursor = (event: MouseEvent, pane: number, line: number, displayStart: number) => {
    event.stopPropagation();
    const target = event.currentTarget as HTMLElement;
    // The content element itself is translated by the horizontal scroll
    // offset, so its bounding box already starts at display column zero.
    const displayColumn = displayStart + Math.max(0, Math.floor((event.clientX - target.getBoundingClientRect().left) / cellWidth));
    void mutate("gui_set_cursor", { pane, line: line - 1, displayColumn });
  };

  const pickerChars = (text: string, matched: number[]) => {
    const selected = new Set(matched);
    return Array.from(text).map((char, index) => ({ char, matched: selected.has(index) }));
  };

  const PaneView = (props: { pane: GuiPane }) => (
    <section
      class="editor-pane"
      data-pane={props.pane.index}
      classList={{ focused: props.pane.focused, single: view().panes.length === 1 }}
      onMouseDown={() => {
        inputSink.focus({ preventScroll: true });
        if (!props.pane.focused) void mutate("gui_focus_pane", { index: props.pane.index });
      }}
    >
      <Show when={view().panes.length > 1}>
        <header class="pane-title">
          <span>{props.pane.fileName}{props.pane.modified ? " •" : ""}</span>
          <small>{props.pane.cursor.line + 1}:{props.pane.cursor.column + 1}</small>
        </header>
      </Show>
      <div class="code-viewport">
        <For each={props.pane.lines}>{(line) => (
          <div class="code-line" classList={{ current: line.current && props.pane.focused }}>
            <span class={`change-mark ${line.git || ""}`} />
            <span class={`diagnostic-mark ${line.diagnostic || ""}`}>{line.diagnostic ? (line.diagnostic === "error" ? "×" : "•") : ""}</span>
            <span class="line-number">{line.continuation ? "" : line.number}</span>
            <span
              class="line-content"
              style={{ transform: `translateX(-${Math.max(0, props.pane.horizontalOffset - line.displayStart) * cellWidth}px)` }}
              onMouseDown={(event) => setCursor(event, props.pane.index, line.number, line.displayStart)}
            >
              <For each={line.segments}>{(segment) => (
                <span
                  class="code-segment"
                  classList={{ cursor: segment.cursor && props.pane.focused, selected: segment.selected, "search-match": segment.searchMatch }}
                  style={{ color: segment.token ? view().theme.syntax[segment.token] : undefined, width: `${segment.cells * cellWidth}px` }}
                >{segment.text}</span>
              )}</For>
            </span>
          </div>
        )}</For>
      </div>
      <div class="overview-ruler">
        <For each={props.pane.lines}>{(line) => <span classList={{ current: line.current && props.pane.focused, diagnostic: Boolean(line.diagnostic), changed: Boolean(line.git) }} />}</For>
      </div>
    </section>
  );

  const PaneTree = (props: { node: GuiLayoutNode }) => (
    <Show
      when={props.node.kind === "split" ? props.node : undefined}
      keyed
      fallback={<PaneView pane={view().panes.find((pane) => pane.index === (props.node.kind === "pane" ? props.node.pane : 0)) || view().panes[0]} />}
    >
      {(split) => (
        <div
          class={`split-layout ${split.direction}`}
          style={split.direction === "vertical"
            ? { "grid-template-columns": `${split.ratio}fr 1px ${1 - split.ratio}fr` }
            : { "grid-template-rows": `${split.ratio}fr 1px ${1 - split.ratio}fr` }}
        >
          <PaneTree node={split.first} />
          <div class="split-separator" />
          <PaneTree node={split.second} />
        </div>
      )}
    </Show>
  );

  const AiPanel = () => (
    <Show when={view().aiChat}>{(chat) => (
      <ChatPanel chat={chat()} focusInput={() => inputSink.focus({ preventScroll: true })} />
    )}</Show>
  );

  const TestPanel = () => (
    <Show when={view().testPanel} keyed>{(test) => (
      <section class="side-panel test-panel" aria-label="Test output">
        <header class="side-panel-header">
          <div><b>{test.scope} tests</b><small>{test.directory}</small></div>
          <span class={`run-status ${test.status}`}>{test.status} · {(test.elapsedMs / 1000).toFixed(1)}s</span>
        </header>
        <div class="run-command">$ {test.command}</div>
        <pre class="output-lines"><Show when={test.truncated}><i>… {test.truncated} earlier lines</i></Show><For each={test.lines}>{(line) => <span>{line}</span>}</For></pre>
        <footer class="panel-summary">{test.summary || "Output updates live"}</footer>
      </section>
    )}</Show>
  );

  const DebugPanel = () => (
    <Show when={view().debug} keyed>{(debug) => (
      <section class="side-panel debug-panel" aria-label="Debugger">
        <header class="side-panel-header"><div><b>Debugger</b><small>{debug.reason || "session active"}</small></div><span>{debug.running ? "running" : "paused"}</span></header>
        <div class="debug-stack"><For each={debug.stack}>{(frame) => <button classList={{ selected: frame.selected }}><b>{frame.name}</b><small>{frame.file}:{frame.line}</small></button>}</For></div>
        <pre class="output-lines"><For each={debug.output}>{(line) => <span>{line}</span>}</For></pre>
      </section>
    )}</Show>
  );

  const SideDock = () => (
    <Show when={view().aiChat || view().testPanel || view().debug}>
      <aside class="side-dock"><AiPanel /><TestPanel /><DebugPanel /></aside>
    </Show>
  );

  const ProblemPanel = () => (
    <Show when={view().problems} keyed>{(problems) => (
      <section class="problem-panel" aria-label={problems.title || "Problems"}>
        <header><b>{problems.title || problems.kind}</b><span>{problems.total} items</span></header>
        <div>
          <For each={problems.items}>{(item) => (
            <button
              classList={{ selected: item.index === problems.selected }}
              onClick={() => void mutate("gui_select_problem", { kind: problems.kind, index: item.index, activate: false })}
              onDblClick={() => void mutate("gui_select_problem", { kind: problems.kind, index: item.index, activate: true })}
            >
              <i class={item.severity}>{item.severity.slice(0, 1).toUpperCase()}</i><strong>{item.message}</strong><small>{item.file}:{item.line}:{item.column}</small>
            </button>
          )}</For>
        </div>
      </section>
    )}</Show>
  );

  const LspOverlay = () => (
    <Show when={!view().aiChat ? view().lspManager : undefined} keyed>{(manager) => (
      <div class="overlay-shade lsp-overlay">
        <section class="lsp-panel">
          <header><div><b>Language servers</b><small>Install, inspect, and manage language intelligence</small></div><kbd>esc</kbd></header>
          <div class="lsp-filter">⌕ {manager.filter || "Filter languages"}</div>
          <div class="lsp-list"><For each={manager.items}>{(item) => (
            <button
              classList={{ selected: item.index === manager.selected }}
              onClick={() => void mutate("gui_select_lsp", { index: item.index, activate: false })}
              onDblClick={() => void mutate("gui_select_lsp", { index: item.index, activate: true })}
            >
              <span class={`server-dot ${item.section.toLowerCase().replaceAll(" ", "-")}`} />
              <strong>{item.language}</strong><small>{item.command || "syntax highlighting"}</small><em>{item.installing || item.state || item.section}</em>
            </button>
          )}</For></div>
        </section>
      </div>
    )}</Show>
  );

  onMount(() => {
    const canvas = document.createElement("canvas");
    const context = canvas.getContext("2d");
    if (context) {
      context.font = getComputedStyle(document.documentElement).getPropertyValue("--editor-font") || "13.5px monospace";
      cellWidth = context.measureText("M").width || FALLBACK_CELL_WIDTH;
    }
    window.addEventListener("keydown", handleKeyDown, { capture: true });
    window.addEventListener("paste", handlePaste);
    window.addEventListener("copy", handleCopy);
    window.addEventListener("cut", handleCut);
    const restoreInputFocus = () => inputSink.focus({ preventScroll: true });
    window.addEventListener("focus", restoreInputFocus);
    editorBody.addEventListener("wheel", handleWheel, { passive: false });
    const observer = new ResizeObserver(syncDimensions);
    observer.observe(editorBody);
    if (native) {
      const snapshots = new Channel<GuiSnapshot>();
      snapshots.onmessage = accept;
      lastDimensions = dimensions();
      void invoke("gui_subscribe", { ...lastDimensions, onEvent: snapshots }).catch((reason) => setError(String(reason)));
    }
    restoreInputFocus();
    onCleanup(() => {
      window.removeEventListener("keydown", handleKeyDown, { capture: true });
      window.removeEventListener("paste", handlePaste);
      window.removeEventListener("copy", handleCopy);
      window.removeEventListener("cut", handleCut);
      window.removeEventListener("focus", restoreInputFocus);
      editorBody.removeEventListener("wheel", handleWheel);
      observer.disconnect();
    });
  });

  return (
    <main class="app" style={themeVars()}>
      <header class="titlebar" data-tauri-drag-region>
        <div class="brand" data-tauri-drag-region><span class="brand-mark">O</span><span>ovim</span></div>
        <div class="window-title" data-tauri-drag-region>
          <span>{view().fileName}{view().modified ? " •" : ""}</span>
          <span class="title-project">— {view().projectName}</span>
        </div>
        <div class="window-actions">
          <button aria-label="Minimize" onClick={() => void windowAction("minimize")}><Icon name="min" /></button>
          <button aria-label="Maximize" onClick={() => void windowAction("toggle-maximize")}><Icon name="max" /></button>
          <button class="window-close" aria-label="Close" onClick={() => void windowAction("close")}><Icon name="close" /></button>
        </div>
      </header>

      <section class="workbench">
        <nav class="activity-bar" aria-label="Primary navigation">
          <div class="activity-main">
            <button classList={{ active: Boolean(view().fileTree) }} title="Explorer  –" onClick={() => void sendLiteral("-")}><Icon name="files" /></button>
            <button title="Search project  Space s g" onClick={() => void sendLiteral(" sg")}><Icon name="search" /></button>
            <button title="Source control"><Icon name="branch" /></button>
            <button title="AI chat  Space Space" onClick={() => { inputSink.focus({ preventScroll: true }); void sendLiteral("  "); }}><Icon name="spark" /></button>
          </div>
          <button title="Settings  :set" onClick={() => void sendLiteral(":set")}><Icon name="gear" /></button>
        </nav>

        <Show when={view().fileTree} keyed>
          {(tree) => (
            <aside class="explorer">
              <div class="panel-heading"><span>Explorer</span><small>{tree.root}</small></div>
              <div class="tree-list">
                <For each={tree.items}>{(item) => (
                  <button
                    class="tree-item"
                    classList={{ selected: item.index === tree.selected }}
                    style={{ "padding-left": `${10 + item.depth * 14}px` }}
                    title={item.path}
                    onClick={() => void mutate("gui_select_file_tree", { index: item.index, activate: false })}
                    onDblClick={() => void mutate("gui_select_file_tree", { index: item.index, activate: true })}
                  >
                    <span class={`tree-chevron ${item.directory ? "directory" : "file"}`}>{item.directory ? (item.expanded ? "⌄" : "›") : ""}</span>
                    <span class={`file-dot ${item.directory ? "folder" : item.name.split(".").pop() || "file"}`} />
                    <span>{item.name}</span>
                  </button>
                )}</For>
              </div>
            </aside>
          )}
        </Show>

        <section class="editor-stack">
          <div class="tabs">
            <For each={view().tabs}>{(tab) => (
              <button class="tab" classList={{ active: tab.active }} onClick={() => void mutate("gui_select_tab", { index: tab.index })}>
                <span class="tab-language">{tab.title.endsWith(".rs") ? "Rs" : "◇"}</span>
                <span>{tab.title}</span>
                <Show when={tab.modified}><span class="modified-dot" /></Show>
              </button>
            )}</For>
            <span class="tabs-fill" />
          </div>

          <div class="breadcrumbs">
            <For each={breadcrumbs()}>{(part, index) => <><span>{part}</span><Show when={index() < breadcrumbs().length - 1}><b>›</b></Show></>}</For>
            <Show when={view().readOnly}><span class="readonly">read only</span></Show>
          </div>

          <div class="editor-body" ref={editorBody!}>
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
            <Show when={composition()}>{(text) => (
              <span
                class="ime-preview"
                style={{
                  top: `${Math.max(0, view().cursor.line - view().firstLine) * LINE_HEIGHT + 8}px`,
                  left: `${Math.max(0, view().cursor.displayColumn - view().horizontalOffset) * cellWidth + 66}px`,
                }}
              >{text()}</span>
            )}</Show>
            <Show when={!view().dashboard} fallback={<Dashboard send={sendLiteral} version="1.2.7" />}>
              <div class="editor-content" classList={{ "has-problems": Boolean(view().problems) }}>
                <div class="primary-content"><div class="pane-tree"><PaneTree node={view().layout} /></div><SideDock /></div>
                <ProblemPanel />
              </div>
            </Show>

            <Show when={!view().aiChat ? view().completion : undefined} keyed>{(menu) => (
              <div class="completion-popover" style={{ top: `${Math.min(58, (view().cursor.line - view().firstLine + 1) * LINE_HEIGHT + 6)}px`, left: `${Math.min(70, (view().cursor.displayColumn - view().horizontalOffset) * cellWidth + 76)}px` }}>
                <For each={menu.items}>{(item) => (
                  <div class="completion-item" classList={{ selected: item.index === menu.selected }}>
                    <span class="completion-kind">{item.kind?.slice(0, 1) || "◇"}</span><strong>{item.label}</strong><small>{item.detail}</small>
                  </div>
                )}</For>
              </div>
            )}</Show>

            <Show when={!view().aiChat ? view().hover : undefined} keyed>{(hover) => (
              <div class="hover-popover"><div class="popover-label">Documentation</div><pre>{hover.content}</pre></div>
            )}</Show>

            <Show when={!view().aiChat ? view().picker : undefined} keyed>{(picker) => (
              <div class="overlay-shade">
                <section class="picker">
                  <header><Icon name="search" /><span>{picker.query || picker.title}</span><kbd>esc</kbd></header>
                  <Show when={picker.fileFilter}><div class="picker-filter">in <strong>{picker.fileFilter}</strong></div></Show>
                  <div class="picker-results">
                    <For each={picker.items}>{(item) => (
                      <button classList={{ selected: item.index === picker.selected }} onClick={() => void mutate("gui_select_picker", { index: item.index })}>
                        <span class="picker-icon">◇</span>
                        <span class="picker-copy">
                          <strong><For each={pickerChars(item.display, item.matched)}>{(part) => <span classList={{ matched: part.matched }}>{part.char}</span>}</For></strong>
                          <small>{item.detail || item.location}</small>
                        </span>
                      </button>
                    )}</For>
                  </div>
                  <footer><span>{picker.total} results</span><span><kbd>↑↓</kbd> navigate <kbd>↵</kbd> open</span></footer>
                </section>
              </div>
            )}</Show>
            <LspOverlay />
          </div>

          <div class="message-line">
            <Show when={view().prompt} keyed fallback={<span class="message">{error() || view().statusMessage || view().lspStatus}</span>}>
              {(prompt) => <div class="prompt"><b>{prompt.prefix}</b><span>{prompt.text}</span><i /></div>}
            </Show>
            <Show when={!connected()}><span class="connecting">connecting…</span></Show>
          </div>

          <footer class="statusbar">
            <div class="mode-chip">{view().mode}</div>
            <div class="status-left">
              <Show when={view().gitBranch}><span><Icon name="branch" />{view().gitBranch}</span></Show>
              <span class="git-counts"><b>+{view().gitChanges.added}</b><i>~{view().gitChanges.modified}</i><em>−{view().gitChanges.removed}</em></span>
              <span classList={{ has: view().diagnostics.errors > 0 }} class="problems">× {view().diagnostics.errors}&nbsp;&nbsp; △ {view().diagnostics.warnings}</span>
            </div>
            <div class="status-right">
              <span>{view().language}</span><span>{view().encoding}</span><span>{view().lineEnding}</span><span>{view().cursor.line + 1}:{view().cursor.column + 1}</span>
            </div>
          </footer>
        </section>
      </section>
    </main>
  );
}

function Dashboard(props: { send: (keys: string) => Promise<void>; version: string }) {
  const shortcuts = [
    [" sf", "Find a file"], [" sg", "Search the project"], ["  ", "Open AI chat"],
    [" tn", "Run nearest test"], [" ca", "Code actions"], ["gd", "Jump to definition"], ["K", "Hover docs"],
  ];
  return <section class="dashboard">
    <div class="dashboard-logo"><span>O</span><div><strong>ovim</strong><small>oxidized, now native</small></div></div>
    <div class="dashboard-rule" />
    <div class="dashboard-shortcuts">
      <For each={shortcuts}>{([keys, label]) => <button onClick={() => void props.send(keys)}><kbd>{keys.replaceAll(" ", "␠")}</kbd><span>{label}</span></button>}</For>
    </div>
    <p>Vim semantics · tree-sitter · LSP · AI <b>v{props.version}</b></p>
  </section>;
}

export default App;
