import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

interface Ad {
  text: string;
  link: string | null;
  suspicious: boolean;
  image: number[] | null;
}

// Same 18 colors as PikoPixel's canvas: ad images are 64x32 indices into it.
const PALETTE = [
  "#ffffff", "#d4d7d9", "#898d90", "#000000",
  "#be0039", "#ff4500", "#ffa800", "#ffd635",
  "#00a368", "#7eed56", "#009eaa", "#2450a4",
  "#3690ea", "#51e9f4", "#811e9f", "#b44ac0",
  "#ff99aa", "#6d482f",
];
const IMAGE_WIDTH = 64;
const IMAGE_HEIGHT = 32;

function AdImage({ image }: { image: number[] }) {
  const ref = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    const ctx = ref.current?.getContext("2d");
    if (!ctx) return;
    for (let i = 0; i < IMAGE_WIDTH * IMAGE_HEIGHT; i++) {
      ctx.fillStyle = PALETTE[image[i]] ?? PALETTE[0];
      ctx.fillRect(i % IMAGE_WIDTH, Math.floor(i / IMAGE_WIDTH), 1, 1);
    }
  }, [image]);
  return <canvas ref={ref} className="sponsored-image" width={IMAGE_WIDTH} height={IMAGE_HEIGHT} />;
}

const POLL_MS = 5 * 60_000;
const ROTATE_MS = 10_000;

// Sponsored text slots rented on PikoPixel by burning PIKO. Nobody reviews
// them, hence the label. The link is shown as plain text on purpose --
// never clickable inside an installed app.
export function SponsoredBanner() {
  const [ads, setAds] = useState<Ad[]>([]);
  const [index, setIndex] = useState(0);

  useEffect(() => {
    const load = () =>
      invoke<Ad[]>("get_active_ads")
        .then(setAds)
        .catch(() => setAds([])); // no banner rather than an error -- mining never depends on this
    load();
    const id = setInterval(load, POLL_MS);
    return () => clearInterval(id);
  }, []);

  useEffect(() => {
    if (ads.length < 2) return;
    const id = setInterval(() => setIndex((i) => i + 1), ROTATE_MS);
    return () => clearInterval(id);
  }, [ads.length]);

  if (ads.length === 0) return null;
  const ad = ads[index % ads.length];
  return (
    <aside className="sponsored-banner" aria-label="Sponsored">
      <span className="sponsored-label">Sponsored · not verified by PIKO · rent this slot on PikoPixel</span>
      {ad.suspicious && (
        <span className="sponsored-warning">⚠ Reported as suspicious by several players -- be extra careful</span>
      )}
      {ad.image && <AdImage image={ad.image} />}
      <span className="sponsored-text">{ad.text}</span>
      {ad.link && <span className="sponsored-link">{ad.link}</span>}
    </aside>
  );
}
