export default defineNuxtConfig({
  ssr: false,
  modules: ["@nuxt/ui"],
  devtools: { enabled: false },
  future: { compatibilityVersion: 4 },
  compatibilityDate: "2025-07-01",

  css: ["@/assets/css/main.css"],

  icon: {
    provider: "iconify",
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
