// OpenCode 2.x only loads a plugin folder that has a server entry. The
// bridge itself lives in tui.js: the server is shared by every terminal,
// so it cannot tell which tmux pane a session belongs to.
export default {
  id: "tmux-agent-sidebar",
  setup() {},
};
