  const helpRows = [
    ["h j k l", "scroll"],
    ["d / u", "half-page down / up"],
    ["gg / G", "top / bottom"],
    ["f / F", "follow link / open in new tab"],
    ["H / L", "back / forward"],
    ["r", "reload"],
    ["o", "focus address"],
    ["t / x", "new / close tab"],
    ["X", "restore closed tab"],
    ["J / K", "previous / next tab"],
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
