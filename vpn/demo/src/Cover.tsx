import React from "react";
import { AbsoluteFill } from "remotion";
import { C, FONT, MONO } from "./ui";

const Lane: React.FC<{ before: React.ReactNode; after: React.ReactNode; tone: "ok" | "bad"; label: string }> = ({ before, after, tone, label }) => (
  <div style={{ display: "flex", alignItems: "center", gap: 28 }}>
    <div style={{ flex: 1, background: C.card, border: `1px solid ${C.line}`, borderRadius: 20, padding: "26px 30px", fontSize: 30, lineHeight: 1.4, boxShadow: "0 12px 34px rgba(0,0,0,0.06)" }}>
      <div style={{ fontSize: 20, color: C.muted, marginBottom: 8, letterSpacing: 0.5 }}>YOUR PROMPT</div>
      {before}
    </div>
    <div style={{ fontSize: 46, color: C.muted }}>→</div>
    <div style={{ flex: 1, background: tone === "ok" ? C.card : "#fbf1f0", border: `1px solid ${tone === "ok" ? C.line : "#efd1cd"}`, borderRadius: 20, padding: "26px 30px", fontSize: 30, lineHeight: 1.4, boxShadow: "0 12px 34px rgba(0,0,0,0.06)" }}>
      <div style={{ fontSize: 20, color: tone === "ok" ? C.green : C.red, marginBottom: 8, letterSpacing: 0.5, fontWeight: 700 }}>{label}</div>
      {after}
    </div>
  </div>
);

const hl = (s: string, bg: string, color = C.ink) => <span style={{ background: bg, color, borderRadius: 8, padding: "1px 8px", fontFamily: MONO, fontSize: 26 }}>{s}</span>;

export const Cover: React.FC = () => (
  <AbsoluteFill style={{ background: C.bg, fontFamily: FONT, color: C.ink, padding: "72px 90px", gap: 46 }}>
    <div style={{ display: "flex", alignItems: "flex-end", justifyContent: "space-between" }}>
      <div>
        <div style={{ fontSize: 92, fontWeight: 800, letterSpacing: -3, lineHeight: 1 }}>prompt vpn</div>
        <div style={{ fontSize: 38, color: C.muted, marginTop: 18 }}>A local privacy proxy for your AI tools. Sensitive data is caught on your machine, before any model sees it.</div>
      </div>
      <div style={{ display: "flex", alignItems: "center", gap: 12, border: `1px solid ${C.line}`, background: C.card, borderRadius: 999, padding: "12px 26px", fontSize: 26, whiteSpace: "nowrap" }}>
        <div style={{ width: 14, height: 14, borderRadius: 7, background: C.green }} />
        Protected · 127.0.0.1:8787
      </div>
    </div>
    <div style={{ display: "flex", flexDirection: "column", gap: 26 }}>
      <Lane
        tone="ok"
        label="WHAT THE MODEL SEES · RESTORED IN THE REPLY"
        before={<>Draft a reply to {hl("maria.gonzalez@example.com", "#ececea")} about her {hl("insulin", "#ececea")} refill</>}
        after={<>Draft a reply to {hl("[[PV_EMAIL_3ce56c70]]", "#e6f0e8", C.green)} about her {hl("[[PV_HEALTH_…]]", "#e6f0e8", C.green)} refill</>}
      />
      <Lane
        tone="bad"
        label="BLOCKED LOCALLY · NOTHING SENT"
        before={<>Update payroll, my SSN is {hl("219-09-9999", "#ececea")} and the key is {hl("sk-proj-Q8z…", "#ececea")}</>}
        after={<span style={{ fontFamily: MONO, fontSize: 26 }}>HTTP 403 PV_POLICY_BLOCKED<br />GOVERNMENT_ID, API_KEY</span>}
      />
    </div>
    <div style={{ display: "flex", gap: 18, flexWrap: "wrap" }}>
      {["Any OpenAI-compatible client", "Rules + checksums + GLiNER, all local", "Your own terms and entity types", "Private NEAR TEE routing, attestation-verified", "Audit log with no prompt text"].map((t) => (
        <div key={t} style={{ background: C.card, border: `1px solid ${C.line}`, borderRadius: 999, padding: "12px 24px", fontSize: 25 }}>{t}</div>
      ))}
    </div>
  </AbsoluteFill>
);
