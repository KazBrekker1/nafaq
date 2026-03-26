export default defineAppConfig({
  ui: {
    colors: {
      primary: "violet",
      neutral: "zinc",
    },
    button: {
      slots: {
        base: "cursor-pointer font-mono font-bold rounded-none",
      },
    },
    input: {
      slots: {
        base: "font-mono rounded-none",
      },
    },
  },
});
