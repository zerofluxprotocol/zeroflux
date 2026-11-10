---
name: promptvpn-setup
description: Deprecated name. Prompt VPN is now ZFX VPN; use the zfx-vpn-setup skill in skills/zfx-vpn-setup/SKILL.md for installing, configuring, upgrading or rolling back Prompt VPN / ZFX VPN.
---

# promptvpn-setup (moved)

This skill was renamed to **zfx-vpn-setup** when Prompt VPN became ZFX VPN.
Follow `skills/zfx-vpn-setup/SKILL.md`.

- The `promptvpn` command still works (it links to `zfx-vpn` and prints a deprecation note).
- Existing Prompt VPN data is migrated by the first `zfx-vpn` command; see "Upgrading from Prompt VPN" in the new skill. `zfx-vpn migrate rollback` undoes it.
- Script paths moved from `skills/promptvpn-setup/scripts/` to `skills/zfx-vpn-setup/scripts/`. `PROMPTVPN_BIN` is still read by the scripts; `ZFX_VPN_BIN` takes precedence.
