#!/bin/sh
# Read-only discovery for the promptvpn-setup skill. Prints JSON.
# Reads no secrets, writes nothing, makes no network calls except to a local
# Ollama on 127.0.0.1 (version probe, 2 s timeout).
set -eu

os=$(uname -s | tr '[:upper:]' '[:lower:]')
arch=$(uname -m)
case "$os" in darwin) os=macos ;; linux) os=linux ;; *) os="unsupported:$os" ;; esac
case "$arch" in arm64|aarch64) arch=arm64 ;; x86_64|amd64) arch=x86_64 ;; *) arch="unsupported:$arch" ;; esac

ram_mb=0
if [ "$os" = macos ]; then
  ram_mb=$(( $(sysctl -n hw.memsize 2>/dev/null || echo 0) / 1048576 ))
elif [ -r /proc/meminfo ]; then
  ram_mb=$(( $(awk '/MemTotal/ {print $2}' /proc/meminfo) / 1024 ))
fi

has() { command -v "$1" >/dev/null 2>&1 && echo true || echo false; }

ollama_installed=$(has ollama)
ollama_version=""
if command -v curl >/dev/null 2>&1; then
  ollama_version=$(curl -s --max-time 2 http://127.0.0.1:11434/api/version 2>/dev/null | sed -n 's/.*"version":"\([^"]*\)".*/\1/p')
fi
ollama_running=false
[ -n "$ollama_version" ] && ollama_running=true

pv_installed=$(has promptvpn)
pv_version=""
[ "$pv_installed" = true ] && pv_version=$(promptvpn --version 2>/dev/null | awk '{print $2}')

client_json() {
  name=$1; bin=$2; cfg=$3
  inst=false; [ "$(has "$bin")" = true ] && inst=true; [ -e "$cfg" ] && inst=true
  cfg_exists=false; [ -e "$cfg" ] && cfg_exists=true
  printf '{"client":"%s","installed":%s,"config_path":"%s","config_exists":%s}' "$name" "$inst" "$cfg" "$cfg_exists"
}

supported=true
case "$os$arch" in *unsupported*) supported=false ;; esac

printf '{\n'
printf '  "os": "%s",\n  "arch": "%s",\n  "supported_platform": %s,\n  "ram_mb": %s,\n' "$os" "$arch" "$supported" "$ram_mb"
printf '  "ollama": {"installed": %s, "running": %s, "version": "%s"},\n' "$ollama_installed" "$ollama_running" "$ollama_version"
printf '  "promptvpn": {"installed": %s, "version": "%s"},\n' "$pv_installed" "$pv_version"
printf '  "clients": [\n    %s,\n    %s,\n    %s,\n    {"client":"env","installed":true,"config_path":"","config_exists":false}\n  ],\n' \
  "$(client_json codex codex "$HOME/.codex/config.toml")" \
  "$(client_json aider aider "$HOME/.aider.conf.yml")" \
  "$(client_json opencode opencode "$HOME/.config/opencode/opencode.json")"
printf '  "notes": "Windows is not supported in release 1. Clients not listed must be pointed at the base URL manually."\n}\n'
