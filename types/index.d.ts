// model: the one chosen at spawn, null when the client's own.
export type MuxAgentsAgent = { name: string; client: string; state: string; doing: string | null; age: number; model: string | null }
export type MuxAgentsBoard = { running: number; agents: MuxAgentsAgent[]; failed: boolean }

declare module 'claude-code' {
  interface PluginState {
    'mux-agents': { board: Shaped<MuxAgentsBoard> }
  }
}
