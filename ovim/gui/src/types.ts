export interface GuiKeyInput {
    key: string;
    shift: boolean;
    control: boolean;
    alt: boolean;
    meta: boolean;
}

export interface GuiSegment {
    text: string;
    cells: number;
    token?: string;
    cursor: boolean;
    selected: boolean;
    searchMatch: boolean;
}

export interface GuiLine {
    number: number;
    continuation: boolean;
    displayStart: number;
    current: boolean;
    segments: GuiSegment[];
    git?: "added" | "modified" | "removed";
    diagnostic?: "error" | "warning" | "information" | "hint";
}

export type GuiLayoutNode =
    | { kind: "pane"; pane: number }
    | {
          kind: "split";
          direction: "horizontal" | "vertical";
          ratio: number;
          first: GuiLayoutNode;
          second: GuiLayoutNode;
      };

export interface GuiPane {
    index: number;
    focused: boolean;
    fileName: string;
    modified: boolean;
    cursor: { line: number; column: number; displayColumn: number };
    firstLine: number;
    scrollSubrow: number;
    horizontalOffset: number;
    totalLines: number;
    lines: GuiLine[];
}

export interface GuiAiProfileOption {
    id: string;
    provider: string;
    model: string;
}

export interface GuiAiChat {
    profile: string;
    profiles: GuiAiProfileOption[];
    reasoningEffort: string;
    reasoningEffortSelection: string;
    reasoningEfforts: string[];
    activity: string;
    waiting: boolean;
    input: string;
    inputCursor: number;
    pendingImages: string[];
    queuedInputs: Array<{
        id: number;
        kind: "steer" | "followUp" | "command";
        content: string;
        imageCount: number;
        hasCodeAttachment: boolean;
        selected: boolean;
    }>;
    setup?: {
        kind: string;
        title: string;
        detail: string;
        maskedInput?: string;
        inputCursor?: number;
        error?: string;
        actions: Array<{ label: string; key: string }>;
    };
    messages: Array<{
        id: string;
        index: number;
        selected: boolean;
        role: string;
        content: string;
        model?: string;
        toolName?: string;
        tools: string[];
    }>;
    streaming?: string;
    streamingThinking?: string;
    thinkingLive: boolean;
    focus: "textInput" | "messageHistory" | "modelSelector" | "treePanel";
    agents: Array<{
        id: string;
        taskName: string;
        lifecycle: string;
        model: string;
        depth: number;
    }>;
    selectedAgentId?: string;
    followedAgentId?: string;
    agentCursor: number;
    approval?: string;
    codeExplanation?: GuiCodeExplanation;
}

export interface GuiCodeExplanation {
    current: number;
    total: number;
    page:
        | { kind: "concept"; title: string; body: string }
        | {
              kind: "code";
              path: string;
              startLine: number;
              endLine: number;
              comment: string;
          };
    discussion:
        | {
              state: "navigating";
              questionCount: number;
              latestQuestion?: string;
              latestAnswer?: string;
              latestFailed: boolean;
          }
        | {
              state: "composing";
              input: string;
              cursor: number;
              questionCount: number;
          }
        | {
              state: "answering";
              question: string;
              answer: string;
              questionCount: number;
          };
}

export interface GuiTestPanel {
    scope: string;
    command: string;
    directory: string;
    status: string;
    elapsedMs: number;
    summary?: string;
    truncated: number;
    lines: string[];
}

export interface GuiProblemList {
    kind: string;
    title: string;
    selected: number;
    total: number;
    items: Array<{
        index: number;
        severity: string;
        file: string;
        line: number;
        column: number;
        message: string;
    }>;
}

export interface GuiLspManager {
    filter: string;
    selected: number;
    showDetail: boolean;
    items: Array<{
        index: number;
        language: string;
        section: string;
        command?: string;
        state?: string;
        installing?: string;
        installHint?: string;
        extensions: string[];
        rootMarkers: string[];
        capabilities: string[];
    }>;
}

export interface GuiDebugPanel {
    running: boolean;
    reason?: string;
    executionLine?: number;
    stack: Array<{
        name: string;
        file: string;
        line: number;
        selected: boolean;
    }>;
    output: string[];
}

export interface GuiTheme {
    name: string;
    background: string;
    foreground: string;
    surface: string;
    surfaceSelected: string;
    border: string;
    accent: string;
    accentForeground: string;
    muted: string;
    cursorLine: string;
    selection: string;
    search: string;
    error: string;
    warning: string;
    info: string;
    success: string;
    syntax: Record<string, string>;
}

export interface GuiSnapshot {
    revision: number;
    mode: string;
    dashboard: boolean;
    filePath?: string;
    fileName: string;
    workspacePath?: string;
    projectName: string;
    language: string;
    encoding: string;
    lineEnding: string;
    modified: boolean;
    readOnly: boolean;
    selectionText?: string;
    cursor: { line: number; column: number; displayColumn: number };
    horizontalOffset: number;
    wrap: boolean;
    tabWidth: number;
    expandTab: boolean;
    firstLine: number;
    totalLines: number;
    lines: GuiLine[];
    layout: GuiLayoutNode;
    panes: GuiPane[];
    tabs: Array<{
        index: number;
        title: string;
        active: boolean;
        modified: boolean;
    }>;
    gitBranch?: string;
    gitChanges: { added: number; modified: number; removed: number };
    diagnostics: {
        errors: number;
        warnings: number;
        information: number;
        hints: number;
    };
    lspStatus: string;
    statusMessage: string;
    prompt?: { prefix: string; text: string; cursor: number };
    picker?: {
        title: string;
        query: string;
        fileFilter?: string;
        selected: number;
        total: number;
        items: Array<{
            index: number;
            display: string;
            location: string;
            detail?: string;
            matched: number[];
        }>;
    };
    completion?: {
        selected: number;
        items: Array<{
            index: number;
            label: string;
            detail?: string;
            kind?: string;
        }>;
    };
    hover?: { content: string; line?: number; displayColumn?: number };
    fileTree?: {
        root: string;
        selected: number;
        items: Array<{
            index: number;
            name: string;
            path: string;
            depth: number;
            directory: boolean;
            expanded: boolean;
        }>;
    };
    aiChat?: GuiAiChat;
    testPanel?: GuiTestPanel;
    problems?: GuiProblemList;
    lspManager?: GuiLspManager;
    debug?: GuiDebugPanel;
    theme: GuiTheme;
    shouldQuit: boolean;
}
