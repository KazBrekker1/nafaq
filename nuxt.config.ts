export default defineNuxtConfig({
  ssr: false,
  modules: ["@nuxt/ui", "@vueuse/nuxt"],
  devtools: { enabled: false },
  future: { compatibilityVersion: 4 },
  compatibilityDate: "2025-07-01",

  runtimeConfig: {
    public: {
      appVersion: process.env.npm_package_version || "0.0.0",
    },
  },

  app: {
    head: {
      viewport: "width=device-width, initial-scale=1, viewport-fit=cover",
      meta: [
        { name: "theme-color", content: "#000000" },
        { name: "format-detection", content: "telephone=no" },
      ],
    },
  },

  css: ["@/assets/css/main.css"],

  // @nuxt/fonts (installed by Nuxt UI) self-hosts Inter at build time.
  // Nuxt UI's default weights stop at 700; font-black needs 900.
  fonts: {
    families: [
      { name: "Inter", provider: "google", weights: [400, 500, 600, 700, 900] },
    ],
  },

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
