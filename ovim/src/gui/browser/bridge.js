(() => {
  if (window.__OVIM_BROWSER_KEY_BRIDGE__) return;

  const nativeOpen = window.open.bind(window);
  const token = "__OVIM_BRIDGE_TOKEN__";
  const keyUrl = intent => `ovim-browser://key/${token}/${intent}`;
  let vimKeysEnabled = __OVIM_VIM_KEYS_ENABLED__;
  const editable = (target) =>
    target instanceof HTMLElement &&
    (target.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(target.tagName));
  const emit = intent => nativeOpen(keyUrl(intent), "_blank");

  Object.defineProperty(window, "__OVIM_BROWSER_KEY_BRIDGE__", {
    value: Object.freeze({
      setVimKeys(controlToken, enabled) {
        if (controlToken !== token) return;
        vimKeysEnabled = Boolean(enabled);
      },
    }),
  });

  document.addEventListener(
    "keydown",
    (event) => {
      if (
        event.defaultPrevented ||
        !event.isTrusted ||
        event.isComposing ||
        event.key !== ":" ||
        !vimKeysEnabled ||
        event.metaKey ||
        event.ctrlKey ||
        event.altKey ||
        editable(event.target)
      )
        return;

      event.preventDefault();
      event.stopImmediatePropagation();
      emit("command");
    },
    true,
  );
})();
