#!/usr/bin/env python3
"""Generate assistant-specific adapters from the canonical skill.

Source of truth: skills/promptvpn-setup/SKILL.md. Outputs:
  adapters/cursor/promptvpn-setup.mdc   (Cursor rule)
  adapters/agents/AGENTS.promptvpn-setup.md  (AGENTS.md-style section for Codex and others)
  adapters/plain/promptvpn-setup.md     (no front matter, for any assistant)
Run: python3 -I scripts/gen_skill_adapters.py  (idempotent; CI checks no diff)
"""
import pathlib
import re
import sys

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else ".").resolve()
skill = root / "skills/promptvpn-setup"
src = (skill / "SKILL.md").read_text()
m = re.match(r"^---\n(.*?)\n---\n(.*)$", src, re.S)
front, body = m.group(1), m.group(2).lstrip()
desc = re.search(r"^description:\s*(.*)$", front, re.M).group(1).strip()
note = "<!-- Generated from skills/promptvpn-setup/SKILL.md by scripts/gen_skill_adapters.py. Do not edit. -->\n"
out = {
    "adapters/cursor/promptvpn-setup.mdc": f"---\ndescription: {desc}\nalwaysApply: false\n---\n{note}\n{body}",
    "adapters/agents/AGENTS.promptvpn-setup.md": f"{note}\n## Skill: promptvpn-setup\n\nWhen to use: {desc}\n\nScripts live in `skills/promptvpn-setup/scripts/`.\n\n{body}",
    "adapters/plain/promptvpn-setup.md": f"{note}\n{body}",
}
for rel, text in out.items():
    p = skill / rel
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(text)
    print("wrote", p.relative_to(root))
