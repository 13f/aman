<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { createNoise2D } from "simplex-noise";

  // ── Configuration ──────────────────────────────────────────────────────

  /**
   * Canvas renders at 1/N of the window size.
   * Must be low enough that aurora bands survive the 28 px backdrop-filter
   * blur applied by the glass layers on top.
   */
  const DOWNSAMPLE = 3;

  /** How fast the aurora drifts. */
  const DRIFT_SPEED = 0.025;

  /**
   * Aurora layers.
   *
   * Peak RGB values are high because the glass layers' `backdrop-filter:
   * blur(28px)` softens everything behind them.  `vStretch` < 1 elongates
   * noise vertically → curtain/ribbon structure.
   */
  const LAYERS = [
    // Green aurora curtain
    { r: 15, g: 110, b: 25,  scale: 1.6, speed: 0.55, vStretch: 0.18 },
    // Teal ribbon
    { r: 8,  g: 85,  b: 80,  scale: 2.2, speed: 0.38, vStretch: 0.14 },
    // Purple fringe
    { r: 80,  g: 8,  b: 70,  scale: 2.8, speed: 0.26, vStretch: 0.25 },
    // Blue ambient
    { r: 8,  g: 35,  b: 90,  scale: 4.2, speed: 0.16, vStretch: 0.32 },
  ];

  // ── Canvas setup ──────────────────────────────────────────────────────

  let canvasEl = $state<HTMLCanvasElement | null>(null);
  let running = true;
  let rafId = 0;
  const noiseFns = LAYERS.map(() => createNoise2D());

  function render(time: number) {
    const canvas = canvasEl;
    if (!canvas) return;

    const w = canvas.width;
    const h = canvas.height;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    const imageData = ctx.createImageData(w, h);
    const data = imageData.data;

    const t = time * 0.001 * DRIFT_SPEED;

    for (let y = 0; y < h; y++) {
      for (let x = 0; x < w; x++) {
        const nx = x / w;
        const ny = y / h;

        let r = 0, g = 0, b = 0;

        for (let i = 0; i < LAYERS.length; i++) {
          const layer = LAYERS[i];
          // Vertical stretch: narrow in x, tall in y → curtains
          const sx = nx * layer.scale / layer.vStretch + t * layer.speed;
          const sy = ny * layer.scale * layer.vStretch + t * layer.speed * 0.5;
          const n = noiseFns[i](sx, sy);
          const wgt = (n + 1) * 0.5;
          r += layer.r * wgt;
          g += layer.g * wgt;
          b += layer.b * wgt;
        }

        const idx = (y * w + x) * 4;
        data[idx]     = Math.min(255, r);
        data[idx + 1] = Math.min(255, g);
        data[idx + 2] = Math.min(255, b);
        data[idx + 3] = 255;
      }
    }

    ctx.putImageData(imageData, 0, 0);
  }

  function tick(now: number) {
    if (!running) return;
    render(now);
    rafId = requestAnimationFrame(tick);
  }

  let reducedMotion = $state(false);
  let mediaQuery: MediaQueryList;

  function setupCanvas(canvas: HTMLCanvasElement) {
    const dpr = window.devicePixelRatio || 1;
    const w = Math.ceil(window.innerWidth / DOWNSAMPLE);
    const h = Math.ceil(window.innerHeight / DOWNSAMPLE);
    canvas.width = w;
    canvas.height = h;
    canvas.style.width = `${window.innerWidth}px`;
    canvas.style.height = `${window.innerHeight}px`;
  }

  function onResize() {
    const canvas = canvasEl;
    if (!canvas) return;
    setupCanvas(canvas);
    // Render one frame immediately so there's no flash
    render(performance.now());
  }

  onMount(() => {
    mediaQuery = window.matchMedia("(prefers-reduced-motion: reduce)");
    reducedMotion = mediaQuery.matches;

    const onChange = (e: MediaQueryListEvent) => {
      reducedMotion = e.matches;
    };
    mediaQuery.addEventListener("change", onChange);

    window.addEventListener("resize", onResize);
    if (canvasEl) {
      setupCanvas(canvasEl);
    }

    if (!reducedMotion) {
      rafId = requestAnimationFrame(tick);
    } else {
      // Render one still frame
      render(0);
    }

    return () => {
      window.removeEventListener("resize", onResize);
      mediaQuery.removeEventListener("change", onChange);
    };
  });

  onDestroy(() => {
    running = false;
    if (rafId) cancelAnimationFrame(rafId);
  });
</script>

<canvas
  bind:this={canvasEl}
  class="aurora-canvas"
  aria-hidden="true"
></canvas>

<style>
  .aurora-canvas {
    position: fixed;
    inset: 0;
    /* Render behind the glass layers (sidebar, main) so the aurora sits
       underneath the frosted glass.  The glass layers' backdrop-filter blur
       softens the noise into natural-looking aurora glow. */
    z-index: 0;
    image-rendering: auto;
    pointer-events: none;
  }
</style>
