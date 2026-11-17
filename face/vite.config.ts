import path from "node:path";
import tailwindcss from "@tailwindcss/vite";
import { tanstackRouter } from "@tanstack/router-plugin/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

// A static page: `npm run build` writes dist/, which flare-server serves
// with --face. In development, /session and /log go to a flare-server
// started with --face (FLARE_SERVER, default the one on this machine).
const server = process.env["FLARE_SERVER"] ?? "http://127.0.0.1:4749";

export default defineConfig({
  plugins: [tanstackRouter({ target: "react", autoCodeSplitting: false }), react(), tailwindcss()],
  resolve: { alias: { "@": path.resolve(__dirname, "./src") } },
  build: { outDir: "dist", emptyOutDir: true, sourcemap: false, assetsDir: "assets" },
  server: {
    proxy: {
      // flare-server lets only its own page attach: to it, the page in
      // development comes from its own address. A browser that loads
      // /session wants the Session screen, not the WebSocket.
      "/session": {
        target: server, ws: true, changeOrigin: true, headers: { origin: server },
        bypass: (request) => (request.headers.upgrade ? undefined : "/index.html"),
      },
      "/log": { target: server, changeOrigin: true },
    },
  },
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["./src/test/setup.ts"],
    include: ["src/**/*.{test,spec}.{ts,tsx}"],
  },
});
