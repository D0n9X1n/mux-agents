// model: the one chosen at spawn, null when the client's own. unread: the caller's unread messages from it. exitCode:
// an ended agent's, null when it has none (a stop, a lost agent, or a list from a binary older than this field).
// activity: a live agent's blocked, done, working or idle; since: seconds since it last changed. Both null when the
// list has none (an ended agent, an older binary, no report).
export type MuxAgentsAgent = {
  name: string; client: string; state: string; doing: string | null; age: number; model: string | null
  unread: number; exitCode: number | null; activity: string | null; since: number | null
}
export type MuxAgentsBoard = { running: number; agents: MuxAgentsAgent[]; failed: boolean }

declare module 'claude-code' {
  interface PluginState {
    'mux-agents': { board: Shaped<MuxAgentsBoard> }
  }
}
