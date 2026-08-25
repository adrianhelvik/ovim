(() => {
  if (window.__OVIM_BROWSER_KEY_BRIDGE__) return;
  window.__OVIM_BROWSER_KEY_BRIDGE__ = true;

  const nativeOpen = window.open.bind(window);
  const commandUrl = "ovim-browser://command/__OVIM_BRIDGE_TOKEN__";
  const editable = (target) =>
    target instanceof HTMLElement &&
    (target.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(target.tagName));

  document.addEventListener(
    "keydown",
    (event) => {
      if (
        event.defaultPrevented ||
        !event.isTrusted ||
        event.isComposing ||
        event.key !== ":" ||
        event.metaKey ||
        event.ctrlKey ||
        event.altKey ||
        editable(event.target)
      )
        return;

      event.preventDefault();
      event.stopImmediatePropagation();
      nativeOpen(commandUrl, "_blank");
    },
    true,
  );
})();
