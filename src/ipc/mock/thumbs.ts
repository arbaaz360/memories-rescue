/**
 * Fake thumbnails for the mock: `mock-thumb:<seed>:<photo|video>` becomes a
 * small SVG landscape as a data: URL (allowed by the CSP's img-src). Real
 * thumbnails are files the engine writes and the window loads through the
 * asset protocol.
 */

const PREFIX = "mock-thumb:";

export function mockThumbPath(seed: number, kind: "photo" | "video"): string {
  return `${PREFIX}${seed}:${kind}`;
}

export function mockThumbnailUrl(path: string): string | null {
  if (!path.startsWith(PREFIX)) return null;
  const [seedText, kind] = path.slice(PREFIX.length).split(":");
  const seed = Number(seedText) || 0;
  const hue = (seed * 47) % 360;
  const sky = `hsl(${hue} 70% 72%)`;
  const skyLow = `hsl(${(hue + 30) % 360} 75% 86%)`;
  const hill = `hsl(${(hue + 140) % 360} 35% 38%)`;
  const hill2 = `hsl(${(hue + 160) % 360} 40% 28%)`;
  const sunX = 18 + ((seed * 13) % 60);
  const play =
    kind === "video"
      ? `<circle cx="48" cy="40" r="13" fill="rgb(0 0 0 / 0.55)"/><path d="M44 33 L56 40 L44 47 Z" fill="#fff"/>`
      : "";
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 96 80"><defs><linearGradient id="g" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="${sky}"/><stop offset="1" stop-color="${skyLow}"/></linearGradient></defs><rect width="96" height="80" fill="url(#g)"/><circle cx="${sunX}" cy="22" r="9" fill="hsl(45 95% 85%)"/><path d="M0 58 Q24 40 48 54 T96 50 V80 H0 Z" fill="${hill}"/><path d="M0 68 Q30 56 60 66 T96 64 V80 H0 Z" fill="${hill2}"/>${play}</svg>`;
  return `data:image/svg+xml,${encodeURIComponent(svg)}`;
}
