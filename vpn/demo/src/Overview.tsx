import React from "react";
import { AbsoluteFill, Easing, interpolate, Series, useCurrentFrame } from "remotion";
import { C, H1, MONO, Pill, Rise, Scene, Shot, Sub, Terminal } from "./ui";
import { BLOCK_LINES, NEAR_LINES, TRANSFORM_LINES } from "./captures";

const clamp = { extrapolateLeft: "clamp", extrapolateRight: "clamp" } as const;

const Hook: React.FC = () => (
  <Scene>
    <Rise>
      <H1 size={118}>Your agent just pasted an SSN into a prompt.</H1>
    </Rise>
    <Rise delay={22}>
      <Sub>Prompt VPN checks every request on your machine before it leaves, and swaps out what shouldn't go.</Sub>
    </Rise>
  </Scene>
);

const Box: React.FC<{ title: string; sub?: string; delay: number; dark?: boolean; width?: number }> = ({ title, sub, delay, dark, width = 360 }) => {
  const f = useCurrentFrame();
  return (
    <div
      style={{
        width,
        padding: "30px 30px",
        borderRadius: 24,
        background: dark ? C.ink : C.card,
        color: dark ? "#fff" : C.ink,
        border: `1px solid ${C.line}`,
        boxShadow: "0 18px 50px rgba(0,0,0,0.08)",
        opacity: interpolate(f, [delay, delay + 15], [0, 1], clamp),
        scale: interpolate(f, [delay, delay + 15], [0.94, 1], { ...clamp, easing: Easing.bezier(0.16, 1, 0.3, 1) }),
      }}
    >
      <div style={{ fontSize: 38, fontWeight: 700 }}>{title}</div>
      {sub ? <div style={{ fontSize: 26, marginTop: 10, color: dark ? "#bdbdbb" : C.muted, lineHeight: 1.35, whiteSpace: "pre-line" }}>{sub}</div> : null}
    </div>
  );
};

const Arrow: React.FC<{ delay: number }> = ({ delay }) => {
  const f = useCurrentFrame();
  return <div style={{ fontSize: 56, color: C.muted, opacity: interpolate(f, [delay, delay + 10], [0, 1], clamp) }}>→</div>;
};

const HowItWorks: React.FC = () => (
  <Scene>
    <Rise>
      <H1 size={84}>One local endpoint. Every request inspected.</H1>
    </Rise>
    <div style={{ display: "flex", alignItems: "center", gap: 26, marginTop: 70 }}>
      <Box title="Your app" sub="Any OpenAI client, agent or SDK" delay={15} width={300} />
      <Arrow delay={30} />
      <Box title="Detect" sub={"Rules + checksums\nYour exact terms\nGLiNER span model"} delay={35} />
      <Arrow delay={50} />
      <Box title="Policy" sub="Allow · placeholder · redact · review · block" delay={55} dark />
      <Arrow delay={70} />
      <Box title="Model" sub="NEAR TEE · local Ollama · your endpoint" delay={75} />
    </div>
    <Rise delay={95}>
      <Sub>Runs on 127.0.0.1. Rules, secrets and placeholder maps never leave the device.</Sub>
    </Rise>
  </Scene>
);

const BlockDemo: React.FC = () => (
  <Scene>
    <Rise>
      <div style={{ display: "flex", alignItems: "baseline", gap: 28, marginBottom: 40 }}>
        <H1 size={78}>Critical data is blocked locally.</H1>
        <Pill tone="bad">nothing sent</Pill>
      </div>
    </Rise>
    <Terminal lines={BLOCK_LINES} start={14} fontSize={29} />
  </Scene>
);

const TransformDemo: React.FC = () => (
  <Scene>
    <Rise>
      <div style={{ display: "flex", alignItems: "baseline", gap: 28, marginBottom: 40 }}>
        <H1 size={70}>Personal data becomes placeholders.</H1>
        <Pill tone="ok">restored in the reply</Pill>
      </div>
    </Rise>
    <Terminal lines={TRANSFORM_LINES} start={14} fontSize={24} />
  </Scene>
);

const ShotScene: React.FC<{ title: string; sub?: string; src: string; srcH: number; from?: number; to?: number; height?: number }> = ({ title, sub, src, srcH, from, to, height = 700 }) => (
  <Scene>
    <Rise>
      <H1 size={70}>{title}</H1>
    </Rise>
    {sub ? (
      <Rise delay={8}>
        <div style={{ fontSize: 36, color: C.muted, marginTop: 16, marginBottom: 34 }}>{sub}</div>
      </Rise>
    ) : (
      <div style={{ height: 40 }} />
    )}
    <Rise delay={12}>
      <Shot src={src} srcH={srcH} from={from} to={to} height={height} />
    </Rise>
  </Scene>
);

const NearScene: React.FC = () => (
  <Scene>
    <Rise>
      <H1 size={74}>Private inference only after the enclave proves itself.</H1>
    </Rise>
    <Rise delay={8}>
      <div style={{ fontSize: 34, color: C.muted, margin: "18px 0 36px" }}>Verified live with NEAR's official SDK: gateway TDX quote bound to TLS, model TDX quote, NVIDIA GPU evidence.</div>
    </Rise>
    <Terminal lines={NEAR_LINES} start={20} fontSize={26} title="promptvpn near verify-attestation" />
  </Scene>
);

const Stat: React.FC<{ big: string; label: string; delay: number }> = ({ big, label, delay }) => (
  <Rise delay={delay} style={{ flex: 1 }}>
    <div style={{ fontSize: 120, fontWeight: 700, letterSpacing: -3 }}>{big}</div>
    <div style={{ fontSize: 34, color: C.muted, marginTop: 8, lineHeight: 1.3 }}>{label}</div>
  </Rise>
);

const Numbers: React.FC = () => (
  <Scene>
    <Rise>
      <H1 size={74}>Measured, not promised.</H1>
    </Rise>
    <div style={{ display: "flex", gap: 60, marginTop: 70 }}>
      <Stat big="102/103" label="critical values caught (synthetic benchmark)" delay={14} />
      <Stat big="89%" label="of names found in free text (26% with rules alone)" delay={28} />
      <Stat big="0.6%" label="of normal prompts held or blocked" delay={42} />
      <Stat big="21 ms" label="median detection time on a laptop CPU" delay={56} />
    </div>
    <Rise delay={80}>
      <div style={{ fontSize: 28, color: C.muted, marginTop: 70 }}>270 synthetic cases. The miss: an SSN spelled out in words. Details in docs/evaluation.md.</div>
    </Rise>
  </Scene>
);

const Outro: React.FC = () => (
  <Scene dark>
    <AbsoluteFill style={{ justifyContent: "center", alignItems: "center", textAlign: "center", padding: 140 }}>
      <Rise>
        <div style={{ fontSize: 120, fontWeight: 700, letterSpacing: -3, color: "#fff" }}>prompt vpn</div>
      </Rise>
      <Rise delay={14}>
        <div style={{ fontFamily: MONO, fontSize: 46, color: "#e8e8e6", marginTop: 40 }}>promptvpn init</div>
      </Rise>
      <Rise delay={26}>
        <div style={{ fontSize: 36, color: "#8a8a88", marginTop: 34 }}>github.com/numinousmuses/promptvpn · macOS and Linux · Apache-2.0</div>
      </Rise>
    </AbsoluteFill>
  </Scene>
);

export const OVERVIEW_FRAMES = 90 + 150 + 260 + 300 + 150 + 165 + 135 + 165 + 230 + 165 + 120;

export const Overview: React.FC = () => (
  <AbsoluteFill style={{ background: C.bg }}>
  <Series>
    <Series.Sequence name="Hook" durationInFrames={90}>
      <Hook />
    </Series.Sequence>
    <Series.Sequence name="How it works" durationInFrames={150}>
      <HowItWorks />
    </Series.Sequence>
    <Series.Sequence name="Block demo" durationInFrames={260}>
      <BlockDemo />
    </Series.Sequence>
    <Series.Sequence name="Transform demo" durationInFrames={300}>
      <TransformDemo />
    </Series.Sequence>
    <Series.Sequence name="Dashboard" durationInFrames={150}>
      <ShotScene title="Every decision is logged. Never the values." src="overview.png" srcH={2498} from={0} to={0.35} />
    </Series.Sequence>
    <Series.Sequence name="Review" durationInFrames={165}>
      <ShotScene title="Unsure? You decide." sub="Held requests are encrypted, shown redacted, and expire in 15 minutes." src="review.png" srcH={2236} from={0} to={0.12} height={640} />
    </Series.Sequence>
    <Series.Sequence name="Onboarding" durationInFrames={135}>
      <ShotScene title="Critical protections are on from the start." src="onboarding-critical.png" srcH={1950} from={0.05} to={0.4} />
    </Series.Sequence>
    <Series.Sequence name="Detection test" durationInFrames={165}>
      <ShotScene title="Add your own kinds of data." sub="Custom entity types are found by the local model and swapped like any other value." src="models-test.png" srcH={6496} from={0.1} to={0.2} height={640} />
    </Series.Sequence>
    <Series.Sequence name="NEAR" durationInFrames={230}>
      <NearScene />
    </Series.Sequence>
    <Series.Sequence name="Numbers" durationInFrames={165}>
      <Numbers />
    </Series.Sequence>
    <Series.Sequence name="Outro" durationInFrames={120}>
      <Outro />
    </Series.Sequence>
  </Series>
  </AbsoluteFill>
);
