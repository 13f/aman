import { Container, Text } from 'pixi.js';

/** Transform warstwy świata: pozycja + skala (przy odbiciu poziomym scale.x=-1). */
export interface LayerTransform {
  x: number;
  y: number;
  scaleX: number;
  scaleY: number;
}

/**
 * Pozycja i skala worldLayer dla danych granic świata. Bez odbicia: kotwiczy lewy
 * brzeg (-minX), skala 1. Z odbiciem: kotwiczy PRAWY brzeg (maxX) i skaluje x=-1 —
 * świat jest lustrzany względem pionowej osi, ale mieści się w tym samym prostokącie.
 */
export function worldLayerTransform(minX: number, maxX: number, minY: number, flipped: boolean): LayerTransform {
  return {
    x: flipped ? maxX : -minX,
    y: -minY,
    scaleX: flipped ? -1 : 1,
    scaleY: 1,
  };
}

/** Punkt sceny (sx,sy) → współrzędne viewportu (uwzględnia odbicie warstwy świata). */
export function worldToViewport(layer: LayerTransform, sx: number, sy: number): { x: number; y: number } {
  return {
    x: layer.x + layer.scaleX * sx,
    y: layer.y + layer.scaleY * sy,
  };
}

/**
 * Odbicie wartości na osi o rozpiętości `extent`: flipped ⇒ extent - value, inaczej
 * value. Wspólny mirror dla minimapy — ten sam wzór odbija X renderowania ORAZ
 * (jako inwolucja: f(f(x))=x) odwraca gx kliknięcia z powrotem na współrzędną świata.
 */
export function flipAxis(value: number, extent: number, flipped: boolean): number {
  return flipped ? extent - value : value;
}

/**
 * Kontr-odbija glify Text w poddrzewie. Gdy warstwa świata ma scale.x=-1, każdy tekst
 * renderowałby się lustrzanie — ustawiamy mu scale.x=-1, więc czyta się normalnie.
 * Rekurencja po kontenerach; Text traktujemy jak liść (nie wchodzimy w jego dzieci).
 */
export function flipTextNodes(node: Container): void {
  for (const child of node.children) {
    if (child instanceof Text) child.scale.x = -1;
    else flipTextNodes(child as Container);
  }
}
