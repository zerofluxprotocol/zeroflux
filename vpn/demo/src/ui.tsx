import React from "react";
import { AbsoluteFill, Easing, Img, interpolate, staticFile, useCurrentFrame, useVideoConfig } from "remotion";

export const C = {
  bg: "#f5f5f4",
  card: "#ffffff",
  ink: "#111111",
  muted: "#6b6b6b",
  line: "#e4e4e2",
  green: "#3f7d4f",
  red: "#a23b32",
  term: "#111111",
  termText: "#e8e8e6",
};
export const FONT = '-apple-system, "SF Pro Display", "Helvetica Neue", Inter, system-ui, sans-serif';
export const MONO = '"SF Mono", Menlo, "JetBrains Mono", monospace';
const ease = Easing.bezier(0.16, 1, 0.3, 1);
const clamp = { extrapolateLeft: "clamp", extrapolateRight: "clamp" } as const;

/** Scene shell: fades in and out over 12 frames. */
export const Scene: React.FC<{ children: React.ReactNode; dark?: boolean }> = ({ children, dark }) => {
  const f = useCurrentFrame();
  const { durationInFrames } = useVideoConfig();
  return (
    <AbsoluteFill
      style={{
        background: dark ? C.term : C.bg,
        fontFamily: FONT,
        color: dark ? C.termText : C.ink,
        opacity: interpolate(f, [0, 12, durationInFrames - 12, durationInFrames], [0, 1, 1, 0], clamp),
        padding: "100px 140px",
        justifyContent: "center",
      }}
    >
      {children}
    </AbsoluteFill>
  );
};

export const Rise: React.FC<{ delay?: number; children: React.ReactNode; style?: React.CSSProperties }> = ({ delay = 0, children, style }) => {
  const f = useCurrentFrame();
  return (
    <div
      style={{
        opacity: interpolate(f, [delay, delay + 18], [0, 1], { ...clamp, easing: ease }),
        translate: interpolate(f, [delay, delay + 18], ["0px 28px", "0px 0px"], { ...clamp, easing: ease }),
        ...style,
      }}
    >
      {children}
    </div>
  );
};

export const H1: React.FC<{ children: React.ReactNode; size?: number }> = ({ children, size = 104 }) => (
  <div style={{ fontSize: size, fontWeight: 700, letterSpacing: -2.5, lineHeight: 1.05 }}>{children}</div>
);

export const Sub: React.FC<{ children: React.ReactNode }> = ({ children }) => (
  <div style={{ fontSize: 46, color: C.muted, lineHeight: 1.3, marginTop: 28, maxWidth: 1500 }}>{children}</div>
);

export type TermLine = { t: string; kind?: "cmd" | "out" | "ok" | "bad" | "dim" };

/** Terminal that types commands and prints real captured output line by line. */
export const Terminal: React.FC<{ lines: TermLine[]; title?: string; start?: number; cps?: number; fontSize?: number }> = ({ lines, title = "zsh", start = 0, cps = 140, fontSize = 30 }) => {
  const f = useCurrentFrame();
  const { fps } = useVideoConfig();
  let t = start;
  const rendered = lines.map((l, i) => {
    const isCmd = (l.kind ?? "out") === "cmd";
    const dur = isCmd ? Math.ceil((l.t.length / cps) * fps) : 4;
    const begin = t;
    t += dur + (isCmd ? 8 : 2);
    if (f < begin) return null;
    const shown = isCmd ? l.t.slice(0, Math.floor(((f - begin) / Math.max(dur, 1)) * l.t.length)) : l.t;
    const color = l.kind === "ok" ? "#8fd19e" : l.kind === "bad" ? "#f08a80" : l.kind === "dim" ? "#8a8a88" : isCmd ? "#ffffff" : C.termText;
    return (
      <div key={i} style={{ color, whiteSpace: "pre-wrap", wordBreak: "break-all" }}>
        {isCmd ? <span style={{ color: "#8a8a88" }}>$ </span> : null}
        {shown}
      </div>
    );
  });
  return (
    <div style={{ background: C.term, borderRadius: 22, boxShadow: "0 30px 80px rgba(0,0,0,0.25)", overflow: "hidden", width: "100%" }}>
      <div style={{ display: "flex", gap: 10, padding: "18px 22px", background: "#1d1d1d", alignItems: "center" }}>
        {["#ff5f57", "#febc2e", "#28c840"].map((c) => (
          <div key={c} style={{ width: 16, height: 16, borderRadius: 8, background: c }} />
        ))}
        <div style={{ color: "#8a8a88", fontFamily: MONO, fontSize: 22, marginLeft: 14 }}>{title}</div>
      </div>
      <div style={{ padding: "26px 34px", fontFamily: MONO, fontSize, lineHeight: 1.5, display: "flex", flexDirection: "column", gap: 6 }}>{rendered}</div>
    </div>
  );
};

/** Real screenshot in a browser-like frame with a slow pan from `from` to `to` (fraction of image height). */
export const Shot: React.FC<{ src: string; height?: number; from?: number; to?: number; zoom?: [number, number]; srcW?: number; srcH: number }> = ({ src, height = 820, from = 0, to = 0, zoom = [1, 1.04], srcW = 2720, srcH }) => {
  const f = useCurrentFrame();
  const { durationInFrames } = useVideoConfig();
  const width = 1640;
  const imgH = (srcH / srcW) * width;
  const maxShift = Math.max(0, imgH - height);
  return (
    <div style={{ width, height, borderRadius: 22, overflow: "hidden", background: C.card, boxShadow: "0 30px 80px rgba(0,0,0,0.14)", border: `1px solid ${C.line}`, alignSelf: "center" }}>
      <Img
        src={staticFile(src)}
        style={{
          width,
          translate: interpolate(f, [0, durationInFrames], [`0px ${-from * maxShift}px`, `0px ${-to * maxShift}px`], { ...clamp, easing: Easing.inOut(Easing.cubic) }),
          scale: interpolate(f, [0, durationInFrames], zoom, clamp),
          transformOrigin: "50% 0%",
        }}
      />
    </div>
  );
};

export const Pill: React.FC<{ children: React.ReactNode; tone?: "ok" | "bad" | "neutral" }> = ({ children, tone = "neutral" }) => (
  <span
    style={{
      display: "inline-block",
      padding: "8px 22px",
      borderRadius: 999,
      fontSize: 30,
      fontWeight: 600,
      background: tone === "ok" ? "#e6f0e8" : tone === "bad" ? "#f6e3e1" : "#ececea",
      color: tone === "ok" ? C.green : tone === "bad" ? C.red : C.ink,
      whiteSpace: "nowrap",
    }}
  >
    {children}
  </span>
);
