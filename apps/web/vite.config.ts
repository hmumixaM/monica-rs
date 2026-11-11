import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { fileURLToPath, URL } from "node:url";
export default defineConfig({
  plugins: [react()],
  resolve: { alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) } },
  server: { proxy: { "/api": process.env.MONICA_API_URL || "http://127.0.0.1:17494" } },
  build: { target: "es2022", sourcemap: false },
  worker: { format: "es" },
});
