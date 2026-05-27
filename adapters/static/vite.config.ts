import { staticAdapter } from "@builder.io/qwik-city/adapters/static/vite";
import { extendConfig } from "@builder.io/qwik-city/vite";
import baseConfig from "../../vite.config";

export default extendConfig(baseConfig, () => {
  return {
    build: {
      ssr: true,
      // Preserve the client build's /build and /assets in dist/ from the
      // first `vite build`. Without this Vite clears dist/ before writing
      // the SSR output, the resulting bundle ships HTML that references
      // /build/q-*.js + /assets/*.css that don't exist, Tauri falls back
      // to index.html (text/html), and Qwik never resumes — UI freezes on
      // the SSR render.
      emptyOutDir: false,
      rollupOptions: {
        input: ["@qwik-city-plan"],
      },
    },
    plugins: [
      staticAdapter({
        origin: "https://tauri.localhost",
      }),
    ],
  };
});
