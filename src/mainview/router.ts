import { createRouter, createWebHashHistory } from "vue-router";

const router = createRouter({
  history: createWebHashHistory(),
  routes: [
    {
      path: "/",
      name: "home",
      component: () => import("./pages/HomePage.vue"),
    },
    {
      path: "/lobby",
      name: "lobby",
      component: () => import("./pages/LobbyPage.vue"),
    },
    {
      path: "/call",
      name: "call",
      component: () => import("./pages/CallPage.vue"),
    },
  ],
});

export default router;
