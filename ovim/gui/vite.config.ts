import { defineConfig } from "vite";
import solid from "vite-plugin-solid";

export default defineConfig({
  plugins: [solid()],
  base: "./",
  build: {
    target: "es2022",
  },
  server: {
    fs: {
      // The shared Strøk icon sprite is repository-level design-system output.
      allow: ["../.."],
    },
  },
});
