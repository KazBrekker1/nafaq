// Height of the visual viewport as a CSS length, tracking the on-screen
// keyboard on mobile so full-height chat layouts keep their input visible.
// Falls back to `fallback` where visualViewport is unavailable.
export function useVisualViewportHeight(fallback: string, onResize?: () => void) {
  const height = ref(fallback);
  const viewport = import.meta.client ? window.visualViewport : null;
  if (viewport) {
    const update = () => {
      height.value = `${viewport.height}px`;
      onResize?.();
    };
    useEventListener(viewport, "resize", update);
    update();
  }
  return height;
}
