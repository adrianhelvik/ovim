/** @vitest-environment jsdom */

import { render, screen } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import manifest from "../../../gui-design-guide/icons/dist/manifest.json";
import { Icon, IconButton } from "./Icon";
import { ICON_NAMES } from "./icons.generated";

describe("Strøk icon registry", () => {
  it("stays synchronized with the generated manifest", () => {
    expect(ICON_NAMES).toEqual(manifest.icons.map((icon) => icon.name));
  });

  it("hides decorative icons and names icon-only controls", () => {
    const result = render(() => <>
      <Icon name="file" />
      <IconButton icon="search" label="Search project" shortcut="Space S G" />
    </>);

    expect(result.container.querySelector("svg")?.getAttribute("aria-hidden")).toBe("true");
    const button = screen.getByRole("button", { name: "Search project" });
    expect(button.title).toBe("Search project · Space S G");
  });
});
