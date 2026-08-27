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

