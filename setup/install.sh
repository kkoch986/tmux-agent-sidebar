#!/usr/bin/env bash
# Wire this sidebar into a machine: Claude Code hooks for every account,
# the OpenCode 2.x bridge, and the tmux options. Safe to re-run.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"

# Claude Code: each config dir (~/.claude, ~/.claude-<account>) has its own
# settings.json, and a session only reports to the sidebar when the plugin
# is enabled in the dir it runs under.
for dir in "$HOME/.claude" "$HOME"/.claude-*; do
  [ -d "$dir" ] || continue
  python3 - "$dir/settings.json" "$REPO" <<'PY'
import json, os, sys
path, repo = sys.argv[1], sys.argv[2]
settings = {}
if os.path.exists(path):
    with open(path) as f:
        settings = json.load(f)
settings.setdefault("enabledPlugins", {})["tmux-agent-sidebar@hiroppy"] = True
settings.setdefault("extraKnownMarketplaces", {})["hiroppy"] = {
    "source": {"source": "directory", "path": repo}
}
tmp = path + ".tmp"
with open(tmp, "w") as f:
    json.dump(settings, f, indent=2)
    f.write("\n")
os.chmod(tmp, 0o600)
os.replace(tmp, path)
PY
  echo "claude: enabled in $dir"
done

# OpenCode 2.x loads the bridge as a plugin folder. The single-file 1.x
# plugin fails to load under 2.x, so an old link to it is replaced.
OPENCODE_PLUGINS="$HOME/.config/opencode/plugins"
if command -v opencode >/dev/null 2>&1; then
  mkdir -p "$OPENCODE_PLUGINS"
  rm -f "$OPENCODE_PLUGINS/tmux-agent-sidebar.js"
  ln -sfn "$REPO/.opencode/plugins/tmux-agent-sidebar" "$OPENCODE_PLUGINS/tmux-agent-sidebar"
  echo "opencode: linked $OPENCODE_PLUGINS/tmux-agent-sidebar"
fi

# tmux: source the personal options ahead of TPM.
line="source-file -q $REPO/setup/tmux.conf"
if ! grep -qF "$line" "$HOME/.tmux.conf" 2>/dev/null; then
  echo
  echo "tmux: add this to ~/.tmux.conf, above the 'run ~/.tmux/plugins/tpm/tpm' line:"
  echo "  $line"
fi
