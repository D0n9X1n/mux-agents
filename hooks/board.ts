// Pure helpers for the Agents pane: spawn detection, feed parsing, cleaning, age text.
import type { MuxAgentsBoard } from '../types/index'

export const EMPTY: MuxAgentsBoard = { running: 0, agents: [], failed: false }

// The entry as a whole file name (after the start, a slash, a backslash, whitespace or a quote), then the verb spawn;
// either may be quoted. Covers `pwsh -File <path> spawn`, `& <path> spawn` and Windows paths.
const ENTRY = /(^|[\\/\s"'])mux-agent\.ps1["']?\s+["']?spawn["']?(\s|$)/i
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

// True when the command ran mux-agent.ps1 spawn and the tool's stdout holds a successful spawn reply.
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
  }))
  return { running: r.running, agents }
}

export function formatAge(seconds: number): string {
  if (seconds < 60) return `${seconds}s`
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m`
  return `${Math.floor(seconds / 3600)}h`
}
