import React from "react";
import { AbsoluteFill, Series } from "remotion";
import { C, H1, Pill, Rise, Scene, Shot, Terminal } from "./ui";
import { ENTITY_LINES } from "./captures";

export const ENTITIES_FRAMES = 120 + 180 + 300 + 150;

export const Entities: React.FC = () => (
  <AbsoluteFill style={{ background: C.bg }}>
  <Series>
    <Series.Sequence name="Intro" durationInFrames={120}>
      <Scene>
        <Rise>
          <H1 size={104}>Your data isn't all SSNs and emails.</H1>
        </Rise>
        <Rise delay={20}>
          <div style={{ fontSize: 46, color: C.muted, marginTop: 28 }}>Describe a kind of data in plain words. The local model finds it.</div>
        </Rise>
      </Scene>
    </Series.Sequence>
    <Series.Sequence name="Add entity" durationInFrames={180}>
      <Scene>
        <Rise>
          <H1 size={70}>Add a custom entity type</H1>
        </Rise>
        <div style={{ height: 36 }} />
        <Rise delay={10}>
          <Shot src="entities.png" srcH={1800} from={0.15} to={0.45} height={680} />
        </Rise>
      </Scene>
    </Series.Sequence>
    <Series.Sequence name="Request" durationInFrames={300}>
      <Scene>
        <Rise>
          <div style={{ display: "flex", alignItems: "baseline", gap: 28, marginBottom: 40 }}>
            <H1 size={74}>Found, swapped, restored.</H1>
            <Pill tone="ok">no rule written</Pill>
          </div>
        </Rise>
        <Terminal lines={ENTITY_LINES} start={14} fontSize={28} />
      </Scene>
    </Series.Sequence>
    <Series.Sequence name="Test" durationInFrames={150}>
      <Scene>
        <Rise>
          <H1 size={70}>Test it before you rely on it.</H1>
        </Rise>
        <div style={{ height: 36 }} />
        <Rise delay={10}>
          <Shot src="models-test.png" srcH={6496} from={0.12} to={0.2} height={680} />
        </Rise>
      </Scene>
    </Series.Sequence>
  </Series>
  </AbsoluteFill>
);
