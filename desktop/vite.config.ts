import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import path from "path";

export default defineConfig({
  plugins: [
    svelte({
      compilerOptions: {
        customElement: true,
      },
    }),
  ],
  resolve: {
    alias: {
      "@shared/frontend": path.resolve(__dirname, "../shared/frontend"),
      // Zustand has optional React peer; Aman uses Svelte.
      react: path.resolve(__dirname, "src/lib/react-stub.ts"),
      "react-dom": path.resolve(__dirname, "src/lib/react-stub.ts"),
    },
  },
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
    fs: {
      allow: ["..", "../shared"],
    },
  },
});
