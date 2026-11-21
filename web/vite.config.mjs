import { fileURLToPath } from "node:url";
import { defineConfig, normalizePath } from "vite";
import react from "@vitejs/plugin-react";

const validators = normalizePath(
  fileURLToPath(new URL("./src/api/generated/validators", import.meta.url)),
);

export default defineConfig({
  plugins: [react()],
  build: {
    rolldownOptions: {
      treeshake: {
        moduleSideEffects: [
          {
            // Generated validators only declare local values and functions.
            test: new RegExp(`^${RegExp.escape(validators)}(?:\\.mjs|/.+\\.mjs)$`),
            sideEffects: false,
          },
        ],
      },
    },
  },
  server: { host: "127.0.0.1" },
});
