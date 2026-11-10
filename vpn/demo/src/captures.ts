// Generated from real captures (see scripts/demo_capture.sh). Do not edit by hand.
import type { TermLine } from './ui';
export const BLOCK_LINES: TermLine[] = [
 {
  "t": "curl localhost:8798/v1/chat/completions -H \"Authorization: Bearer $PVK\" -d '{\"model\":\"gpt-5\",\"messages\":[{\"role\":\"user\",\"content\":\"Update payroll for my account. My SSN is 219-09-9999.\"}]}'",
  "kind": "cmd"
 },
 {
  "t": "HTTP 403  PV_POLICY_BLOCKED  categories=['GOVERNMENT_ID']",
  "kind": "bad"
 },
 {
  "t": "\"Request blocked by local policy\"  event 534a9cc3...",
  "kind": "dim"
 },
 {
  "t": "curl localhost:8798/v1/chat/completions -H \"Authorization: Bearer $PVK\" -d '{\"model\":\"gpt-5\",\"messages\":[{\"role\":\"user\",\"content\":\"Deploy with OPENAI_API_KEY=sk-proj-Q8zR3mPx9Tv2Wn7Ks4Yb6HdQ8zR3mPx9Tv2Wn7Ks\"}]}'",
  "kind": "cmd"
 },
 {
  "t": "HTTP 403  PV_POLICY_BLOCKED  categories=['API_KEY']",
  "kind": "bad"
 },
 {
  "t": "\"Request blocked by local policy\"  event 9367befe...",
  "kind": "dim"
 }
];
export const TRANSFORM_LINES: TermLine[] = [
 {
  "t": "curl localhost:8798/v1/chat/completions -H \"Authorization: Bearer $PVK\" -d '{\"model\":\"gpt-5\",\"messages\":[{\"role\":\"user\",\"content\":\"Draft a reply to maria.gonzalez@example.com about her refund.\"}]}'",
  "kind": "cmd"
 },
 {
  "t": "# what the model received:",
  "kind": "dim"
 },
 {
  "t": "Draft a reply to [[PV_EMAIL_3ce56c70]] about her refund.",
  "kind": "ok"
 },
 {
  "t": "# what your app got back (test model echoes its input):",
  "kind": "dim"
 },
 {
  "t": "You said: Draft a reply to maria.gonzalez@example.com about her refund.",
  "kind": "out"
 },
 {
  "t": "curl localhost:8798/v1/chat/completions -H \"Authorization: Bearer $PVK\" -d '{\"model\":\"gpt-5\",\"messages\":[{\"role\":\"user\",\"content\":\"remind maria gonzalez her insurance policy number is HX-2291-0042, she has been managing her insulin dose\"}]}'",
  "kind": "cmd"
 },
 {
  "t": "# what the model received:",
  "kind": "dim"
 },
 {
  "t": "remind maria gonzalez her insurance policy number is [[PV_CUSTOM_c049d5db]], she has been managing her [[PV_HEALTH_INFORMATION_1e5c317d]] dose",
  "kind": "ok"
 },
 {
  "t": "# what your app got back (test model echoes its input):",
  "kind": "dim"
 },
 {
  "t": "You said: remind maria gonzalez her insurance policy number is HX-2291-0042, she has been managing her insulin dose",
  "kind": "out"
 }
];
export const NEAR_LINES: TermLine[] = [
 {
  "t": "promptvpn near verify-attestation deepseek-ai/DeepSeek-V4.1-Flash-1017",
  "kind": "cmd"
 },
 {
  "t": "VERIFIED: deepseek-ai/DeepSeek-V4.1-Flash-1017 is eligible for PRIVATE_ROUTE",
  "kind": "ok"
 },
 {
  "t": "gateway signer 40599fb284f43953bb356b09080fd803c960fc10285ad90eca462ca711ad4acb (TCB UpToDate), TLS SPKI 73862c254372091cc6895b900111503860694f8916ebf0c63b18d245ba8ba5ad",
  "kind": "out"
 },
 {
  "t": "model signer 236f5a6b91dd47dc3b7b36e43d4eb531a042ffb8f8fbf3a1555b78cef4fdda4c (TCB UpToDate), GPU evidence verified, E2EE key present",
  "kind": "out"
 },
 {
  "t": "verifier: nearai-inference-sdk 0.3.0 (gpu=required, x-no-aliasing)",
  "kind": "out"
 },
 {
  "t": "promptvpn near verify-attestation deepseek-ai/DeepSeek-V4-Flash",
  "kind": "cmd"
 },
 {
  "t": "NOT ELIGIBLE: deepseek-ai/DeepSeek-V4-Flash: 'deepseek-ai/DeepSeek-V4-Flash' is an alias of 'z-ai/glm-5.3-flash'; select the canonical model explicitly",
  "kind": "bad"
 }
];
export const ENTITY_LINES: TermLine[] = [
 {
  "t": "promptvpn entity add --label \"legal case number\"",
  "kind": "cmd"
 },
 {
  "t": "added entity type ent_34c9e46945",
  "kind": "ok"
 },
 {
  "t": "curl localhost:8798/v1/chat/completions -H \"Authorization: Bearer $PVK\" -d '{\"model\":\"gpt-5\",\"messages\":[{\"role\":\"user\",\"content\":\"Summarize the filing for legal case number 2024-CV-01187 before the hearing\"}]}'",
  "kind": "cmd"
 },
 {
  "t": "# what the model received:",
  "kind": "dim"
 },
 {
  "t": "Summarize the filing for legal case number [[PV_CUSTOM_0aedea23]] before the hearing",
  "kind": "ok"
 },
 {
  "t": "# what your app got back (test model echoes its input):",
  "kind": "dim"
 },
 {
  "t": "You said: Summarize the filing for legal case number 2024-CV-01187 before the hearing",
  "kind": "out"
 }
];
