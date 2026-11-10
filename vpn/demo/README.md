# Demo videos

Remotion project for the Prompt VPN demo videos. Rendered output lives in `docs/demo/`.

Everything on screen is real: terminal lines come from `src/captures.ts`, generated from actual runs of the `promptvpn` binary (`scripts/demo_capture.sh`), and UI shots are screenshots of the dashboard served by the daemon (`apps/dashboard/scripts/screenshots.mjs`). The upstream model in the terminal scenes is the bundled mock, which echoes what it received; that is how the videos show exactly what left the machine.

```sh
npm i
npx remotion studio                                   # preview
npx remotion render Overview ../docs/demo/promptvpn-overview.mp4 --crf=24
npx remotion render CustomEntities ../docs/demo/promptvpn-custom-entities.mp4 --crf=24
```
