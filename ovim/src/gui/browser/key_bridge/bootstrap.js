(() => {
  if (window.__OVIM_BROWSER_KEY_BRIDGE__) return;

  const nativeOpen = window.open.bind(window);
  const commandToken = "__OVIM_BRIDGE_TOKEN__";
  const stateChannel = "ovim-browser-keys:__OVIM_STATE_TOKEN__";
  const isRootFrame = window === window.top;
  const reducedMotion = window.matchMedia?.(
    "(prefers-reduced-motion: reduce)",
  ).matches;
  const hintAlphabet = "sadfjklewcmpgh";
  const sharedState = {
    enabled: __OVIM_VIM_KEYS_ENABLED__,
    mode: "normal",
  };

  let countBuffer = "";
  let sequence = "";
  let sequenceTimer;
  let hintSession;
  let helpOverlay;
  let findOverlay;
  let findQuery = "";
  let toastTimer;

  const directFrames = () => Array.from(window.frames);
  const isDirectChild = source =>
    directFrames().some(frame => frame === source);
  const sendToFrames = message => {
    for (const frame of directFrames()) frame.postMessage(message, "*");
  };
  const stateMessage = () => ({
    channel: stateChannel,
    kind: "state",
    enabled: sharedState.enabled,
    mode: sharedState.mode,
  });

  const commandUrl = (intent, count = 1, url) => {
    const query = new URLSearchParams();
    if (count !== 1) query.set("count", String(count));
    if (url) query.set("url", url);
    const suffix = query.size ? "?" + query.toString() : "";
    return (
      "ovim-browser://key/" +
      commandToken +
      "/" +
      intent +
      suffix
    );
  };
  const emit = (intent, count = 1, url) =>
    nativeOpen(commandUrl(intent, count, url), "_blank");

  const prevent = event => {
    event.preventDefault();
    event.stopImmediatePropagation();
  };
  const clearSequence = () => {
    countBuffer = "";
    sequence = "";
    window.clearTimeout(sequenceTimer);
    sequenceTimer = undefined;
  };
  const beginSequence = prefix => {
    sequence = prefix;
    window.clearTimeout(sequenceTimer);
    sequenceTimer = window.setTimeout(clearSequence, 1200);
  };
  const takeCount = () => {
    const count = Math.max(
      1,
      Math.min(Number.parseInt(countBuffer || "1", 10), 100),
    );
    clearSequence();
    return count;
  };

  const editableRoles = new Set(["textbox", "searchbox", "combobox", "spinbutton"]);
  const editableElement = node =>
    node instanceof HTMLElement &&
    (node.isContentEditable ||
      /^(INPUT|TEXTAREA|SELECT)$/.test(node.tagName) ||
      editableRoles.has(node.getAttribute("role")) ||
      node.dataset.ovimBrowserOverlay === "find");
  const deepActiveElement = () => {
    let active = document.activeElement;
    while (active instanceof HTMLElement && active.shadowRoot?.activeElement)
      active = active.shadowRoot.activeElement;
    return active;
  };
  const editableInPath = event =>
    event.composedPath().some(editableElement) ||
    editableElement(deepActiveElement());

  const removeHints = () => {
    hintSession?.host.remove();
    hintSession = undefined;
  };
  const removeHelp = () => {
    helpOverlay?.remove();
    helpOverlay = undefined;
  };
  const removeFind = () => {
    findOverlay?.remove();
    findOverlay = undefined;
  };
  const clearTransientUi = () => {
    removeHints();
    removeHelp();
    removeFind();
  };

  const applySharedState = (enabled, mode, relay = true) => {
    sharedState.enabled = Boolean(enabled);
    sharedState.mode =
      sharedState.enabled && mode === "insert" ? "insert" : "normal";
    clearSequence();
    if (!sharedState.enabled || sharedState.mode === "insert")
      clearTransientUi();
    if (relay) sendToFrames(stateMessage());
  };

  const requestMode = mode => {
    if (sharedState.mode === mode || !sharedState.enabled) return;
    applySharedState(sharedState.enabled, mode);
    emit(mode === "insert" ? "mode_insert" : "mode_normal");
    if (!isRootFrame)
      window.parent.postMessage(
        { channel: stateChannel, kind: "mode", mode },
        "*",
      );
  };

  window.addEventListener("message", event => {
    const message = event.data;
    if (!message || message.channel !== stateChannel) return;
    if (message.kind === "state" && event.source === window.parent) {
      applySharedState(message.enabled, message.mode);
      return;
    }
    if (message.kind === "request" && isDirectChild(event.source)) {
      event.source.postMessage(stateMessage(), "*");
      return;
    }
    if (message.kind === "mode" && isDirectChild(event.source)) {
      applySharedState(sharedState.enabled, message.mode);
      if (!isRootFrame)
        window.parent.postMessage(
          { channel: stateChannel, kind: "mode", mode: message.mode },
          "*",
        );
    }
  });

  if (!isRootFrame)
    window.parent.postMessage(
      { channel: stateChannel, kind: "request" },
      "*",
    );

  Object.defineProperty(window, "__OVIM_BROWSER_KEY_BRIDGE__", {
    configurable: false,
    value: Object.freeze({
      setVimKeys(controlToken, enabled) {
        if (controlToken !== commandToken) return;
        applySharedState(enabled, "normal");
      },
      run(controlToken, action) {
        if (controlToken !== commandToken) return;
        if (action === "find") showFind();
      },
    }),
    writable: false,
  });

  const scrollContainer = () => {
    let candidate =
      document.activeElement instanceof HTMLElement
        ? document.activeElement
        : undefined;
    while (candidate && candidate !== document.body) {
      const style = getComputedStyle(candidate);
      if (
        /(auto|scroll)/.test(style.overflowY + style.overflowX) &&
        (candidate.scrollHeight > candidate.clientHeight ||
          candidate.scrollWidth > candidate.clientWidth)
      )
        return candidate;
      candidate = candidate.parentElement ?? undefined;
    }
    return document.scrollingElement || document.documentElement;
  };
  const scrollBy = (left, top) =>
    scrollContainer().scrollBy({
      left,
      top,
      behavior: reducedMotion ? "auto" : "smooth",
    });
  const scrollToEdge = edge => {
    const target = scrollContainer();
    target.scrollTo({
      top: edge === "top" ? 0 : target.scrollHeight,
      behavior: reducedMotion ? "auto" : "smooth",
    });
  };

  const visible = element => {
    if (!(element instanceof HTMLElement) || element.hidden) return false;
    const style = getComputedStyle(element);
    if (style.visibility === "hidden" || style.display === "none") return false;
    const rect = element.getBoundingClientRect();
    return (
      rect.width > 1 &&
      rect.height > 1 &&
      rect.bottom > 0 &&
      rect.right > 0 &&
      rect.top < innerHeight &&
      rect.left < innerWidth
    );
  };
  const actionableElements = () =>
    Array.from(
      document.querySelectorAll(
        [
          "a[href]",
          "button:not([disabled])",
          "input:not([disabled]):not([type=hidden])",
          "select:not([disabled])",
          "textarea:not([disabled])",
          "[role=button]",
          "[role=link]",
          "[onclick]",
          "[tabindex]:not([tabindex='-1'])",
        ].join(","),
      ),
    )
      .filter(visible)
      .slice(0, 400);

  const hintLabel = (index, width) => {
    let value = index;
    let label = "";
    for (let place = 0; place < width; place += 1) {
      label = hintAlphabet[value % hintAlphabet.length] + label;
      value = Math.floor(value / hintAlphabet.length);
    }
    return label;
  };
  const overlayHost = name => {
    const host = document.createElement("div");
    host.dataset.ovimBrowserOverlay = name;
    host.style.cssText =
      "position:fixed;inset:0;z-index:2147483647;pointer-events:none;";
    document.documentElement.append(host);
    return { host, root: host.attachShadow({ mode: "closed" }) };
  };
