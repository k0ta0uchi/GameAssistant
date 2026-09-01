import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// https://vitejs.dev/config/
export default defineConfig(async () => ({
  plugins: [react(), tailwindcss()],

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: "127.0.0.1",
    watch: {
      // 3. tell vite to ignore watching backend/data directories so DB writes don't trigger page reloads
      ignored: [
        /[\\/]src-tauri[\\/]/,
        /[\\/]data[\\/]/,
        /[\\/]logs[\\/]/,
        /[\\/]backups[\\/]/,
        /[\\/]models[\\/]/,
        /[\\/]tools[\\/]/,
        /[\\/]scripts[\\/]/,
        /[\\/]\.gemini[\\/]/,
        /[\\/]\.gemini_workflow[\\/]/,
        /[\\/]chroma.*[\\/]/,
        /[\\/]target[\\/]/,
        /\.lance($|[\\/])/,
        /\.txn$/,
        /\.manifest$/,
        /latest_version_hint\.json$/,
        /settings\.json$/,
        (filePath: string) => {
          const normalized = filePath.replace(/\\/g, "/");
          return (
            normalized.includes("/data/") ||
            normalized.startsWith("data/") ||
            normalized.includes("/src-tauri/") ||
            normalized.startsWith("src-tauri/") ||
            normalized.includes("/logs/") ||
            normalized.startsWith("logs/") ||
            normalized.includes("/models/") ||
            normalized.startsWith("models/") ||
            normalized.includes("/scripts/") ||
            normalized.startsWith("scripts/") ||
            normalized.endsWith(".lance") ||
            normalized.endsWith(".txn") ||
            normalized.endsWith(".manifest") ||
            normalized.endsWith("settings.json")
          );
        },
      ],
    },
  },
}));
