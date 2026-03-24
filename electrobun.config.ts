import type { ElectrobunConfig } from "electrobun";

export default {
  app: {
    name: "Nafaq",
    identifier: "com.nafaq.app",
    version: "0.1.0",
  },
  runtime: {
    exitOnLastWindowClosed: true,
  },
  build: {
    bun: {
      entrypoint: "src/bun/index.ts",
    },
    views: {
      mainview: {
        entrypoint: "src/mainview/index.ts",
      },
    },
    copy: {
      "dist/index.html": "views/mainview/index.html",
      "dist/assets": "views/mainview/assets",
    },
    watchIgnore: ["dist/**", "sidecar/**"],
  },
  scripts: {
    postWrap: "./scripts/post-wrap.ts",
  },
} satisfies ElectrobunConfig;
