export default defineNuxtConfig({
  ssr: false,
  modules: ["@nuxt/ui"],
  devtools: { enabled: false },
  future: { compatibilityVersion: 4 },
  compatibilityDate: "2025-07-01",

  app: {
    head: {
      viewport: "width=device-width, initial-scale=1, viewport-fit=cover",
    },
  },

  css: ["@/assets/css/main.css"],

  icon: {
    clientBundle: {
      scan: true,
    },
  },

  vite: {
    envPrefix: ["VITE_", "TAURI_"],
    server: {
      strictPort: true,
      hmr: {
        protocol: "ws",
        host: "localhost",
        port: 3001,
      },
      watch: {
        ignored: ["**/src-tauri/**"],
      },
    },
  },
});
