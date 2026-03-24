import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  plugins: [vue(), tailwindcss()],
  root: "src/mainview",
  build: {
    outDir: "../../dist",
    emptyOutDir: true,
    target: "esnext",
    rollupOptions: {
      external: ["electrobun/view"],
    },
  },
  server: {
    port: 5173,
    strictPort: true,
  },
});
