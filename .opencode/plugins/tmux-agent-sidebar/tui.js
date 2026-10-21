// OpenCode 2.x bridge for tmux-agent-sidebar.
//
// It runs as a CLI (terminal UI) plugin on purpose. OpenCode 2.x shares one
// background server between every terminal, so a server plugin only ever
// sees the $TMUX_PANE of whichever terminal started that server. The TUI
// runs in the pane itself, so its $TMUX_PANE is right, and it only forwards
// events for the session this terminal is showing.
import { spawn } from "node:child_process";
import { appendFileSync, existsSync, realpathSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const resolveHookScript = () => {
  const here = fileURLToPath(import.meta.url);
  const starts = [dirname(here)];
  try {
    starts.push(dirname(realpathSync(here)));
  } catch {}
  for (const start of starts) {
    let dir = start;
    for (let i = 0; i < 5; i += 1) {
      const candidate = resolve(dir, "hook.sh");
      if (existsSync(candidate)) return candidate;
      const parent = dirname(dir);
      if (parent === dir) break;
      dir = parent;
    }
  }
  const tpm = resolve(homedir(), ".tmux/plugins/tmux-agent-sidebar/hook.sh");
  return existsSync(tpm) ? tpm : null;
};

const HOOK_SCRIPT = resolveHookScript();
const DEBUG_LOG = process.env.TMUX_AGENT_SIDEBAR_DEBUG;

const debug = (line) => {
  if (!DEBUG_LOG) return;
  try {
    appendFileSync(DEBUG_LOG, `${new Date().toISOString()} ${line}\n`);
  } catch {}
};

// Fire-and-forget, like the Claude hooks: the sidebar must never slow the
// terminal down or break it when the binary is missing.
const hook = (eventName, payload) => {
  if (!process.env.TMUX_PANE) return;
  debug(`hook ${eventName} ${JSON.stringify(payload).slice(0, 200)}`);
  try {
    const [cmd, args] = HOOK_SCRIPT
      ? ["bash", [HOOK_SCRIPT, "opencode", eventName]]
      : ["tmux-agent-sidebar", ["hook", "opencode", eventName]];
    const child = spawn(cmd, args, { stdio: ["pipe", "ignore", "ignore"] });
    child.on("error", () => {});
    child.stdin.on("error", () => {});
    child.stdin.end(JSON.stringify(payload));
  } catch {}
};

// The TUI wraps bus events as `{ name, details: { type, data } }`; the
// server stream uses `{ type, data }`. Accept either.
const typeOf = (event) => event?.name ?? event?.details?.type ?? event?.type;
const payloadOf = (event) =>
  event?.details?.data ?? event?.data ?? event?.properties ?? {};

const errorText = (error) => {
  if (!error) return "";
  if (typeof error === "string") return error;
  return error.message || error.type || JSON.stringify(error);
};

const unquote = (s) => (typeof s === "string" ? s.replace(/^"(.*)"$/s, "$1") : "");

export default {
  id: "tmux-agent-sidebar",
  setup(context) {
    const cwd = context.location?.directory ?? process.cwd();
    const currentSession = () => {
      try {
        const route = context.ui?.router?.current?.();
        return route?.type === "session" ? route.sessionID : null;
      } catch {
        return null;
      }
    };

    // Per session: last reply text and tool names by call id, since a
    // tool's name and its result arrive on different events.
    const lastReply = new Map();
    const toolNames = new Map();
    let announced = null;

    debug(`setup pane=${process.env.TMUX_PANE} hook=${HOOK_SCRIPT}`);

    const onEvent = (event) => {
      const type = typeOf(event);
      if (!type) return;
      const data = payloadOf(event);
      const session_id = data.sessionID;
      if (!session_id) return;
      const current = currentSession();
      debug(`event ${type} session=${session_id} current=${current}`);
      // A brand-new session's first events can land before the router
      // switches to it, so the inbox event that carries the prompt is
      // accepted while this terminal is not yet showing any session.
      const forThisPane =
        session_id === current || (current === null && type === "session.inbox.enqueued");
      if (!forThisPane) return;

      if (announced !== session_id) {
        announced = session_id;
        hook("session-start", { cwd, session_id, source: "startup" });
      }

      switch (type) {
        case "session.inbox.enqueued": {
          const text = unquote(data.item?.payload?.text);
          if (data.item?.type === "user") {
            hook("user-prompt-submit", { cwd, session_id, prompt: text });
          }
          return;
        }
        case "session.execution.started":
          hook("user-prompt-submit", { cwd, session_id, prompt: "" });
          return;
        case "session.text.ended":
          if (data.text) lastReply.set(session_id, data.text);
          return;
        case "session.execution.succeeded":
        case "session.execution.interrupted":
          hook("stop", { cwd, session_id, last_message: lastReply.get(session_id) ?? "" });
          return;
        case "session.execution.failed":
          hook("stop-failure", {
            cwd,
            session_id,
            error: errorText(data.error) || "session failed",
          });
          return;
        case "session.tool.input.started":
          toolNames.set(data.id, data.name);
          return;
        case "session.tool.called":
          hook("activity-log", {
            cwd,
            session_id,
            tool_name: toolNames.get(data.id) ?? data.name ?? "",
            tool_input: data.input ?? {},
            tool_response: {},
          });
          return;
        case "permission.asked":
          hook("notification", { cwd, session_id, wait_reason: "permission_prompt" });
          return;
        case "permission.replied":
          hook("user-prompt-submit", { cwd, session_id, prompt: "" });
          return;
        default:
          // Questions and forms the agent puts to the user surface as
          // `*.asked` events; anything else is not the sidebar's business.
          if (type.endsWith(".asked")) {
            hook("notification", { cwd, session_id, wait_reason: "elicitation_dialog" });
          }
      }
    };

    const listen = context.data?.listen;
    if (typeof listen !== "function") {
      debug("context.data.listen missing; bridge inactive");
      return;
    }
    return listen.call(context.data, onEvent);
  },
};
