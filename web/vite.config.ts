import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Relative base so the built index.html references ./assets/... and works no
// matter what path the Rust server mounts it under.
export default defineConfig({
  base: "./",
  plugins: [react()],
  build: { outDir: "dist", emptyOutDir: true },
  server: { host: true, port: 5173, proxy: { "/video": "http://127.0.0.1:8080" } },
});
