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
    revision: number;
    sessions: BrowserSession[];
    activeSessionId?: string;
    maxSessions: number;
    presentationRequest?: {
        revision: number;
        sessionId: string;
    };
}

export type BrowserToolbarAction =
    "back" | "forward" | "reload" | "stop" | "focus" | "find";

export interface BrowserBounds {
    x: number;
    y: number;
    width: number;
    height: number;
    visible: boolean;
}

export interface BrowserAddressFocusRequest {
    serial: number;
    sessionId: string;
}
