import { readFile, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

const manifestUrl = new URL(
  "../../../gui-design-guide/icons/dist/manifest.json",
  import.meta.url,
);
const outputUrl = new URL("../src/icons.generated.ts", import.meta.url);
const manifest = JSON.parse(await readFile(manifestUrl, "utf8"));
const names = manifest.icons.map((icon) => icon.name);
const lines = names.map((name) => `  ${JSON.stringify(name)},`).join("\n");
const source = `// Generated from gui-design-guide/icons/dist/manifest.json.
// Run \`npm run icons:generate\` after changing the Strøk icon source set.
export const ICON_NAMES = [
${lines}
] as const;

export type IconName = (typeof ICON_NAMES)[number];
`;

await writeFile(outputUrl, source);
console.log(`Generated ${names.length} icon names in ${fileURLToPath(outputUrl)}`);
