import { fileURLToPath } from "node:url";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "vite";

const root = fileURLToPath(new URL(".", import.meta.url));

export default defineConfig({
  root,
  base: "./",
  plugins: [
    tailwindcss(),
    {
      name: "luvus-development-csp",
      apply: "serve",
      transformIndexHtml(html) {
        return html.replace("connect-src 'self';", "connect-src 'self' ws://127.0.0.1:5173;");
      },
    },
  ],
  server: {
    host: "127.0.0.1",
    port: 5173,
    strictPort: true,
    cors: false,
    fs: {
      strict: true,
      allow: [fileURLToPath(new URL("../..", import.meta.url))],
    },
  },
  preview: {
    host: "127.0.0.1",
    port: 4173,
    strictPort: true,
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    sourcemap: false,
    target: "es2023",
  },
});
