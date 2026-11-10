#!/usr/bin/env python3
"""Generate assistant-specific adapters from the canonical skill.

Source of truth: skills/zfx-vpn-setup/SKILL.md. Outputs:
  adapters/cursor/zfx-vpn-setup.mdc   (Cursor rule)
  adapters/agents/AGENTS.zfx-vpn-setup.md  (AGENTS.md-style section for Codex and others)
  adapters/plain/zfx-vpn-setup.md     (no front matter, for any assistant)
Run: python3 -I scripts/gen_skill_adapters.py  (idempotent; CI checks no diff)
"""
import pathlib
import re
import sys

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else ".").resolve()
skill = root / "skills/zfx-vpn-setup"
src = (skill / "SKILL.md").read_text()
m = re.match(r"^---\n(.*?)\n---\n(.*)$", src, re.S)
front, body = m.group(1), m.group(2).lstrip()
desc = re.search(r"^description:\s*(.*)$", front, re.M).group(1).strip()
note = "<!-- Generated from skills/zfx-vpn-setup/SKILL.md by scripts/gen_skill_adapters.py. Do not edit. -->\n"
out = {
    "adapters/cursor/zfx-vpn-setup.mdc": f"---\ndescription: {desc}\nalwaysApply: false\n---\n{note}\n{body}",
    "adapters/agents/AGENTS.zfx-vpn-setup.md": f"{note}\n## Skill: zfx-vpn-setup\n\nWhen to use: {desc}\n\nScripts live in `skills/zfx-vpn-setup/scripts/`.\n\n{body}",
    "adapters/plain/zfx-vpn-setup.md": f"{note}\n{body}",
}
for rel, text in out.items():
    p = skill / rel
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(text)
    print("wrote", p.relative_to(root))
