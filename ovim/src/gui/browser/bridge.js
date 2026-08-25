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

  const editableInPath = event =>
    event.composedPath().some(
      node =>
        node instanceof HTMLElement &&
        (node.isContentEditable ||
          /^(INPUT|TEXTAREA|SELECT)$/.test(node.tagName)),
    );

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

  const startHints = newTab => {
    removeHints();
    const elements = actionableElements();
    if (!elements.length) return;
    let width = 1;
    while (hintAlphabet.length ** width < elements.length) width += 1;
    const { host, root } = overlayHost("hints");
    const style = document.createElement("style");
    style.textContent =
      ".hint{position:fixed;padding:2px 4px;border:1px solid #1b2030;" +
      "border-radius:3px;background:#ffcf40;color:#111827;" +
      "font:700 11px/1.2 ui-monospace,monospace;box-shadow:0 1px 4px #0008;" +
      "text-transform:uppercase}.hint.dim{display:none}";
    root.append(style);
    const hints = elements.map((element, index) => {
      const rect = element.getBoundingClientRect();
      const label = hintLabel(index, width);
      const marker = document.createElement("span");
      marker.className = "hint";
      marker.textContent = label;
      marker.style.left = Math.max(0, rect.left + 2) + "px";
      marker.style.top = Math.max(0, rect.top + 2) + "px";
      root.append(marker);
      return { element, label, marker };
    });
    hintSession = { host, hints, prefix: "", newTab };
  };

  const handleHintKey = event => {
    if (!hintSession) return false;
    if (event.key === "Escape") {
      prevent(event);
      removeHints();
      return true;
    }
    const key = event.key.toLowerCase();
    if (!hintAlphabet.includes(key)) {
      prevent(event);
      return true;
    }
    prevent(event);
    hintSession.prefix += key;
    const matches = hintSession.hints.filter(hint =>
      hint.label.startsWith(hintSession.prefix),
    );
    for (const hint of hintSession.hints)
      hint.marker.classList.toggle("dim", !matches.includes(hint));
    const match = matches.find(
      hint => hint.label === hintSession.prefix,
    );
    if (!match) {
      if (!matches.length) removeHints();
      return true;
    }
    const openInTab = hintSession.newTab;
    removeHints();
    if (
      openInTab &&
      match.element instanceof HTMLAnchorElement &&
      match.element.href
    )
      emit("new_tab", 1, match.element.href);
    else {
      match.element.focus({ preventScroll: true });
      match.element.click();
    }
    return true;
  };

  const helpRows = [
    ["h j k l", "scroll"],
    ["d / u", "half-page down / up"],
    ["gg / G", "top / bottom"],
    ["f / F", "follow link / open in new tab"],
    ["H / L", "back / forward"],
    ["r", "reload"],
    ["o", "focus address"],
    ["t / x", "new / close tab"],
    ["J / K", "next / previous tab"],
    ["gt / gT", "next / previous tab"],
    ["g0 / g$", "first / last tab"],
    ["gi", "focus an input"],
    ["/ · n / N", "find · next / previous"],
    ["yy", "copy page address"],
    ["i / Esc", "Insert / Normal mode"],
    [":", "Ovim browser commands"],
    ["count + key", "repeat, up to 100"],
  ];
  const showHelp = () => {
    if (helpOverlay) {
      removeHelp();
      return;
    }
    const { host, root } = overlayHost("help");
    host.style.pointerEvents = "auto";
    const style = document.createElement("style");
    style.textContent =
      ".backdrop{position:absolute;inset:0;display:grid;place-items:center;" +
      "background:#080a10b8;font:13px/1.4 system-ui,sans-serif;color:#d8def0}" +
      ".card{width:min(560px,calc(100vw - 40px));max-height:calc(100vh - 40px);" +
      "overflow:auto;border:1px solid #39415c;border-radius:10px;background:#111521;" +
      "box-shadow:0 18px 70px #000b}.head{display:flex;justify-content:space-between;" +
      "padding:16px 18px;border-bottom:1px solid #2b3248}.head strong{color:#7dd3fc}" +
      ".head span{color:#8e99b7}.grid{display:grid;grid-template-columns:max-content 1fr;" +
      "gap:8px 22px;padding:16px 18px}.key{font:600 12px ui-monospace,monospace;" +
      "color:#f8fafc}.desc{color:#aeb8d2}";
    const backdrop = document.createElement("div");
    backdrop.className = "backdrop";
    const card = document.createElement("div");
    card.className = "card";
    const head = document.createElement("div");
    head.className = "head";
    const title = document.createElement("strong");
    title.textContent = "Ovim browser keys";
    const close = document.createElement("span");
    close.textContent = "? or Esc to close";
    head.append(title, close);
    const grid = document.createElement("div");
    grid.className = "grid";
    for (const [keys, description] of helpRows) {
      const key = document.createElement("span");
      key.className = "key";
      key.textContent = keys;
      const desc = document.createElement("span");
      desc.className = "desc";
      desc.textContent = description;
      grid.append(key, desc);
    }
    card.append(head, grid);
    backdrop.append(card);
    root.append(style, backdrop);
    backdrop.addEventListener("click", event => {
      if (event.target === backdrop) removeHelp();
    });
    helpOverlay = host;
  };

  const showToast = text => {
    document.querySelector("[data-ovim-browser-overlay=toast]")?.remove();
    window.clearTimeout(toastTimer);
    const { host, root } = overlayHost("toast");
    const note = document.createElement("div");
    note.textContent = text;
    note.style.cssText =
      "position:fixed;right:18px;bottom:18px;padding:8px 11px;" +
      "border:1px solid #39415c;border-radius:6px;background:#111521;" +
      "color:#d8def0;font:12px system-ui,sans-serif;box-shadow:0 8px 28px #0008";
    root.append(note);
    toastTimer = window.setTimeout(() => host.remove(), 1600);
  };

  const runFind = (backwards = false) => {
    if (!findQuery || typeof window.find !== "function") return;
    window.find(findQuery, false, backwards, true, false, true, false);
  };
  const showFind = () => {
    removeFind();
    const { host, root } = overlayHost("find");
    host.style.pointerEvents = "auto";
    const form = document.createElement("form");
    form.style.cssText =
      "position:fixed;top:12px;right:12px;display:flex;align-items:center;" +
      "gap:8px;padding:7px 9px;border:1px solid #39415c;border-radius:7px;" +
      "background:#111521;box-shadow:0 8px 28px #0009";
    const input = document.createElement("input");
    input.type = "search";
    input.placeholder = "Find on page";
    input.value = findQuery;
    input.style.cssText =
      "width:230px;border:0;outline:0;background:transparent;color:#f8fafc;" +
      "font:13px system-ui,sans-serif";
    const hint = document.createElement("span");
    hint.textContent = "Enter · Esc";
    hint.style.cssText =
      "color:#8e99b7;font:11px ui-monospace,monospace;white-space:nowrap";
    form.append(input, hint);
    root.append(form);
    input.addEventListener("input", () => {
      findQuery = input.value;
      runFind(false);
    });
    input.addEventListener("keydown", event => {
      if (event.key === "Escape") {
        event.preventDefault();
        removeFind();
      } else if (event.key === "Enter") {
        event.preventDefault();
        findQuery = input.value;
        runFind(event.shiftKey);
      }
    });
    form.addEventListener("submit", event => event.preventDefault());
    findOverlay = host;
    input.focus({ preventScroll: true });
    input.select();
  };

  const focusInput = count => {
    const inputs = Array.from(
      document.querySelectorAll(
        "input:not([disabled]):not([type=hidden]),textarea:not([disabled])," +
          "select:not([disabled]),[contenteditable=true]",
      ),
    ).filter(visible);
    const target = inputs[Math.min(count - 1, inputs.length - 1)];
    if (target instanceof HTMLElement) {
      target.focus({ preventScroll: false });
      if (
        target instanceof HTMLInputElement ||
        target instanceof HTMLTextAreaElement
      )
        target.select();
    }
  };

  const handleSequence = (event, key) => {
    if (sequence === "g") {
      prevent(event);
      const count = takeCount();
      if (key === "g") scrollToEdge("top");
      else if (key === "t") emit("next_tab", count);
      else if (key === "T") emit("previous_tab", count);
      else if (key === "0") emit("first_tab");
      else if (key === "$") emit("last_tab");
      else if (key === "i") focusInput(count);
      return true;
    }
    if (sequence === "y") {
      prevent(event);
      takeCount();
      if (key === "y")
        navigator.clipboard
          ?.writeText(location.href)
          .then(() => showToast("Address copied"))
          .catch(() => showToast("Could not copy address"));
      return true;
    }
    return false;
  };

  const handleNormalKey = event => {
    const key = event.key;
    if (handleHintKey(event)) return;
    if (helpOverlay && (key === "?" || key === "Escape")) {
      prevent(event);
      removeHelp();
      return;
    }
    if (key === "Escape") {
      if (sequence || countBuffer || findOverlay) {
        prevent(event);
        clearSequence();
        removeFind();
      }
      return;
    }
    if (event.metaKey || event.ctrlKey || event.altKey) return;
    if (editableInPath(event)) return;
    if (handleSequence(event, key)) return;
    if (/^[0-9]$/.test(key) && (countBuffer || key !== "0")) {
      prevent(event);
      countBuffer = (countBuffer + key).slice(0, 3);
      return;
    }

    const count = () => takeCount();
    switch (key) {
      case "g":
      case "y":
        prevent(event);
        beginSequence(key);
        break;
      case ":":
        prevent(event);
        count();
        emit("command");
        break;
      case "h":
        prevent(event);
        scrollBy(-60 * count(), 0);
        break;
      case "j":
        prevent(event);
        scrollBy(0, 60 * count());
        break;
      case "k":
        prevent(event);
        scrollBy(0, -60 * count());
        break;
      case "l":
        prevent(event);
        scrollBy(60 * count(), 0);
        break;
      case "d":
        prevent(event);
        scrollBy(0, innerHeight * 0.5 * count());
        break;
      case "u":
        prevent(event);
        scrollBy(0, innerHeight * -0.5 * count());
        break;
      case "G":
        prevent(event);
        count();
        scrollToEdge("bottom");
        break;
      case "H":
        prevent(event);
        emit("back", count());
        break;
      case "L":
        prevent(event);
        emit("forward", count());
        break;
      case "J":
        prevent(event);
        emit("next_tab", count());
        break;
      case "K":
        prevent(event);
        emit("previous_tab", count());
        break;
      case "r":
        prevent(event);
        count();
        emit("reload");
        break;
      case "o":
        prevent(event);
        count();
        emit("focus_address");
        break;
      case "t":
        prevent(event);
        emit("new_tab", count());
        break;
      case "x":
        prevent(event);
        count();
        emit("close_tab");
        break;
      case "i":
        prevent(event);
        count();
        requestMode("insert");
        break;
      case "f":
      case "F":
        prevent(event);
        count();
        startHints(key === "F");
        break;
      case "?":
        prevent(event);
        count();
        showHelp();
        break;
      case "/":
        prevent(event);
        count();
        showFind();
        break;
      case "n":
      case "N":
        prevent(event);
        for (let index = 0, times = count(); index < times; index += 1)
          runFind(key === "N");
        break;
      default:
        clearSequence();
    }
  };

  document.addEventListener(
    "keydown",
    event => {
      if (
        event.defaultPrevented ||
        !event.isTrusted ||
        event.isComposing ||
        event.key === "Process" ||
        event.key === "Dead" ||
        !sharedState.enabled
      )
        return;
      if (sharedState.mode === "insert") {
        if (event.key === "Escape") {
          prevent(event);
          requestMode("normal");
        }
        return;
      }
      handleNormalKey(event);
    },
    true,
  );
})();
