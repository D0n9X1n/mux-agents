export type MuxAgentsAgent = { name: string; client: string; state: string; doing: string | null; age: number }
export type MuxAgentsBoard = { running: number; agents: MuxAgentsAgent[]; failed: boolean }

declare module 'claude-code' {
  interface PluginState {
    'mux-agents': { board: Shaped<MuxAgentsBoard> }
  }
}
