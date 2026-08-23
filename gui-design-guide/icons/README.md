# Ovim Strøk icon set

This directory is the executable icon specification for the GUI.

- src contains the editable Strøk source of truth.
- dist contains themeable SVG, 16/20/24px dark review renders, a sprite, a manifest, and the contact sheet.
- The family currently contains 26 icons on a 24×24 grid.

## Review

Open [the contact sheet](dist/contact-sheet.png) for family weight and [the manifest](dist/manifest.json) for names and meanings. Inspect individual PNGs at 100% zoom; scaling the window defeats the smallest-size test.

The current family was reviewed on a #090b12 dark canvas with #c8d3f5 glyphs and on a light #f5f7fb canvas with #172033 glyphs. Shipping SVGs retain currentColor.

## Regenerate

~~~sh
strok batch gui-design-guide/icons/src \
  --out gui-design-guide/icons/dist \
  --sizes 16,20,24 \
  --color '#c8d3f5' \
  --bg '#090b12' \
  --sprite gui-design-guide/icons/dist/ovim-icons.svg \
  --manifest gui-design-guide/icons/dist/manifest.json \
  --sheet gui-design-guide/icons/dist/contact-sheet.png \
  --columns 6
~~~

Run Strøk audit after geometry changes:

~~~sh
for icon_file in gui-design-guide/icons/src/*.strok; do
  strok -f "$icon_file" audit
done
~~~

The repeat hints currently reported for Explorer nodes and Settings rails are already expressed as repeat blocks in source; Strøk’s audit observes the expanded render graph.

## Integration recommendation

Use the generated sprite during the migration:

~~~html
<svg aria-hidden="true" width="20" height="20">
  <use href="/icons/ovim-icons.svg#explorer"></use>
</svg>
~~~

For a packaged Tauri application, import or copy the sprite through Vite so the asset URL is stable under the configured relative base. Generate the TypeScript icon-name union from manifest.json and keep the SVG decorative when the containing control owns the accessible label.
