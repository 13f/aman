<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { createNoise2D } from "simplex-noise";

  // ── Configuration ──────────────────────────────────────────────────────

  /** Downsample factor — canvas renders at 1/N resolution for performance. */
  const DOWNSAMPLE = 6;

  /** How fast the aurora drifts (higher = faster). */
  const DRIFT_SPEED = 0.04;

  /** Base colours (RGB, 0–255) for the three noise layers. */
  const LAYERS = [
    { r: 22, g: 28, b: 70, scale: 2.5, speed: 0.7 },   // deep navy
    { r: 38, g: 14, b: 55, scale: 3.5, speed: 0.5 },   // dark plum
    { r: 14, g: 20, b: 60, scale: 4.5, speed: 0.35 },  // deep indigo
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
          const n = noiseFns[i](
            nx * layer.scale + t * layer.speed,
            ny * layer.scale + t * layer.speed * 0.7,
          );
          // Map noise [-1, 1] → weight [0, 1]
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
    z-index: 0;
    /* The canvas is already low-res; let the browser upscale with
       bilinear filtering for a naturally soft look. */
    image-rendering: auto;
    pointer-events: none;
  }
</style>
