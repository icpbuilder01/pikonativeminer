import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

interface Ad {
  text: string;
  link: string | null;
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
      <span className="sponsored-text">{ad.text}</span>
      {ad.link && <span className="sponsored-link">{ad.link}</span>}
    </aside>
  );
}
