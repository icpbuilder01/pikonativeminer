import { useEffect, useState } from "react";

interface Piece {
  id: number;
  left: number;
  color: string;
  delay: number;
  duration: number;
  rotate: number;
  drift: number;
}

const COLORS = ["#3987e5", "#eb6834", "#1baf7a", "#eda100", "#e87ba4", "#9085e9"];

function randomBurst(seed: number): Piece[] {
  return Array.from({ length: 72 }, (_, i) => ({
    id: seed * 1000 + i,
    left: Math.random() * 100,
    color: COLORS[i % COLORS.length],
    delay: Math.random() * 0.15,
    duration: 1.6 + Math.random() * 0.9,
    rotate: Math.random() * 360,
    drift: (Math.random() - 0.5) * 140,
  }));
}

// A trigger key change (any new positive number) fires one confetti burst.
export function Confetti({ trigger }: { trigger: number }) {
  const [pieces, setPieces] = useState<Piece[]>([]);

  useEffect(() => {
    if (trigger === 0) return;
    // Randomness and the DOM burst it drives belong here (an effect), not in
    // render -- generating it lazily in a deferred callback keeps this an
    // "external system" update rather than a synchronous render-time one.
    const showTimeout = setTimeout(() => setPieces(randomBurst(trigger)), 0);
    const hideTimeout = setTimeout(() => setPieces([]), 2800);
    return () => {
      clearTimeout(showTimeout);
      clearTimeout(hideTimeout);
    };
  }, [trigger]);

  if (pieces.length === 0) return null;

  return (
    <div className="confetti-root" aria-hidden="true">
      <div className="win-flash" key={trigger} />
      {pieces.map((p) => (
        <span
          key={p.id}
          className="confetti-piece"
          style={{
            left: `${p.left}%`,
            background: p.color,
            animationDelay: `${p.delay}s`,
            animationDuration: `${p.duration}s`,
            // @ts-expect-error custom property read by the keyframe in App.css
            "--drift": `${p.drift}px`,
            "--rotate": `${p.rotate}deg`,
          }}
        />
      ))}
    </div>
  );
}
