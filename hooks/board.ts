// Pure helpers for the Agents pane: spawn detection, feed parsing, cleaning, age text.
import type { MuxAgentsAgent, MuxAgentsBoard } from '../types/index'

export const EMPTY: MuxAgentsBoard = { running: 0, agents: [], failed: false }

// The entry as a whole file name (after the start, a slash, a backslash, whitespace or a quote): the shim mux-agent,
// mux-agent.cmd, the binary mux-agent.exe or the old mux-agent.ps1; then the verb spawn. Either may be quoted. Covers
// `<path> spawn`, `sh <path> spawn`, `cmd.exe /d /c <path> spawn`, `& <path> spawn`, Windows paths and a 0.1 command
// line that ran mux-agent.ps1 with `-File`.
const ENTRY = /(^|[\\/\s"'])mux-agent(\.ps1|\.cmd|\.exe)?["']?\s+["']?spawn["']?(\s|$)/i
// Format-MuxAgentResult -Text for a successful spawn: ok and verb are left out, the record's fields are printed in order.
// A refusal prints code, check and message instead, so it never matches.
const TEXT_SPAWN = /^name: .+\nid: [0-9a-f]{12}\npane: %\d+\nwindow: @\d+\nclient: (claude|copilot)\nsessionId: .*\ncontext: .+\nreason: .+$/m

// One reply object with ok true and verb spawn. Each line is parsed on its own, so ok and verb from two replies never
// combine.
function isJsonSpawnReply(line: string): boolean {
  if (!line.startsWith('{')) return false
  let reply: unknown
  try { reply = JSON.parse(line) } catch { return false }
  const r = reply as { ok?: unknown; verb?: unknown } | null
  return r !== null && typeof r === 'object' && r.ok === true && r.verb === 'spawn'
}

// A successful spawn reply: a JSON line from the entry, or the -Text lines of a successful spawn.
export function isSpawnReply(output: string): boolean {
  const text = output.replace(/\r\n/g, '\n')
  return text.split('\n').some(line => isJsonSpawnReply(line.trim())) || TEXT_SPAWN.test(text)
}

// True when the command ran the entry's spawn and the tool's stdout holds a successful spawn reply.
export function isSpawn(command: string, stdout: string | undefined): boolean {
  return ENTRY.test(command) && isSpawnReply(stdout ?? '')
}

// What the pane may draw: tabs become spaces; C0, DEL and C1 controls go.
export function clean(text: string): string {
  return text.replace(/\t/g, ' ').replace(/[\u0000-\u001f\u007f-\u009f]/g, '')
}

// The board from one `list -All` run, every string cleaned, or undefined when the run or its JSON failed.
export function parseList(exitCode: number, stdout: string): Omit<MuxAgentsBoard, 'failed'> | undefined {
  if (exitCode !== 0) return undefined
  let reply: unknown
  try { reply = JSON.parse(stdout) } catch { return undefined }
  const r = reply as { ok?: unknown; running?: unknown; agents?: unknown }
  if (r?.ok !== true || typeof r.running !== 'number' || !Array.isArray(r.agents)) return undefined
  const agents = r.agents.map((a: Record<string, unknown>) => ({
    name: clean(String(a.name ?? '')), client: clean(String(a.client ?? '')), state: clean(String(a.state ?? '')),
    doing: typeof a.doing === 'string' ? clean(a.doing) : null, age: typeof a.age === 'number' ? a.age : 0,
    model: typeof a.model === 'string' ? clean(a.model) : null, unread: typeof a.unread === 'number' ? a.unread : 0,
    exitCode: typeof a.exitCode === 'number' ? a.exitCode : null,
  }))
  return { running: r.running, agents }
}

// How a row is drawn, as the side bar draws it (src/sidebar.rs Kind): its glyph, Unicode or ASCII (MUX_AGENTS_ASCII=1),
// its state word and their color, undefined for dim. `ended` dims the row.
export type Look = { glyph: string; word: string; color: 'green' | 'red' | 'yellow' | undefined; ended: boolean }

// A lost agent's pane is gone, so it is not drawn, as on the side bar. An exit with no code (a list from an older
// binary) is a dim `- exited`: it is neither proof of done nor of a failure. Fields are read tolerantly, since a
// stored board may lack them.
export function look(a: MuxAgentsAgent, ascii: boolean): Look | undefined {
  const code = typeof a.exitCode === 'number' ? a.exitCode : undefined
  switch (a.state) {
    case 'running': return { glyph: ascii ? '*' : '\u25c9', word: 'working', color: 'green', ended: false }
    case 'lost': return undefined
    case 'exited':
      if (code === 0) return { glyph: ascii ? '+' : '\u2713', word: 'done', color: undefined, ended: true }
      if (code !== undefined) return { glyph: ascii ? 'x' : '\u2717', word: `exit ${code}`, color: 'red', ended: true }
      return { glyph: '-', word: 'exited', color: undefined, ended: true }
    case 'stopped': return { glyph: '-', word: 'stopped', color: 'yellow', ended: true }
    default: return { glyph: '-', word: a.state, color: undefined, ended: true }
  }
}

// The side bar's unread badge, `✉ N` (ASCII `m N`), only for a count above 0.
export function mail(unread: unknown, ascii: boolean): string | undefined {
  if (typeof unread !== 'number' || unread <= 0) return undefined
  return `${ascii ? 'm' : '\u2709'} ${unread}`
}

export function formatAge(seconds: number): string {
  if (seconds < 60) return `${seconds}s`
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m`
  return `${Math.floor(seconds / 3600)}h`
}
