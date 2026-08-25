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
