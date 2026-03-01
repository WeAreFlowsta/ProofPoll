import { defineConfig, type UserConfig } from "vite";
import { qwikVite } from "@builder.io/qwik/optimizer";
import { qwikCity } from "@builder.io/qwik-city/vite";
import tsconfigPaths from "vite-tsconfig-paths";
import tailwindcss from "@tailwindcss/vite";

// Tauri expects a fixed port — use 5174 to avoid conflict with Flowsta Vault (5173)
const TAURI_DEV_PORT = 5174;

export default defineConfig((): UserConfig => {
  return {
    plugins: [
      qwikCity(),
      qwikVite(),
      tsconfigPaths({ root: "." }),
      tailwindcss(),
    ],
    server: {
      port: TAURI_DEV_PORT,
      strictPort: true,
      headers: {
        "Cache-Control": "public, max-age=0",
      },
      watch: {
        ignored: ["**/src-tauri/**"],
      },
    },
    define: {
      __APP_VERSION__: JSON.stringify(
        process.env.npm_package_version ?? "0.1.0"
      ),
    },
    build: {
      target: "esnext",
      outDir: "dist",
    },
    preview: {
      headers: {
        "Cache-Control": "public, max-age=600",
      },
    },
  };
});
