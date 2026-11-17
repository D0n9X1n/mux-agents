// What this session's own client is doing, reported for the side bar as herdr's agents report theirs (OSC 7501):
// working while a turn runs, blocked while it asks the person, idle once it ends. Only an agent's own pane or hc
// terminal is answered; anywhere else `report` refuses `target`, `herdr` or `tagged` and this session stops asking.
import type { EngineInterface, Register } from 'claude-code'

import { turnCompleted, turnStarted } from './deliver'

type On = Parameters<Register>[0]
type State = 'working' | 'blocked' | 'idle'

// Per module load: the reports in the order their events came, one at a time, so a slow one never lands after a
// later one; and whether this session is an agent's (unknown until the first answer).
let queue: Promise<void> = Promise.resolve()
let agent: boolean | undefined

// The entry shim with `tail`. Hooks have no `process`, so the OS comes from the environment.
async function entryArgv($: EngineInterface, tail: string[]): Promise<string[]> {
  const root = $.plugin.root
  let os
  try { os = await $.env.get('OS') } catch { os = undefined }
  if (os === 'Windows_NT') return ['cmd.exe', '/d', '/c', `${root}\\scripts\\mux-agent.cmd`, ...tail]
  return ['sh', `${root}/scripts/mux-agent`, ...tail]
}

// The entry shim's `report`.
async function reportArgv($: EngineInterface, state: State, kind?: string): Promise<string[]> {
  return entryArgv($, kind ? ['report', state, '-Kind', kind] : ['report', state])
}

// Inside tmux, rmux or an hc terminal: a place that could be an agent's, as deliver.ts inTeam. A plain herdr
// terminal is refused `herdr` on its first report and asks no more.
async function inMux($: EngineInterface): Promise<boolean> {
  try {
    return Boolean((await $.env.get('HC_TERMINAL_ID')) || (await $.env.get('TMUX')) || (await $.env.get('RMUX')))
  } catch {
    return false
  }
}

// The refusals that mean this place is no agent's (or no longer is): `target` (no agent here), `herdr` (a plain herdr
// terminal), `tagged` (not proven a member). After one, no more reports.
const NOT_AGENT = /"check":"(target|herdr|tagged)"/

// One report, after every earlier one. Every other failure is dropped and the next event reports again.
async function send($: EngineInterface, state: State, kind?: string) {
  if (agent === false || !(await inMux($))) return
  try {
    const ran = await $.process.run(await reportArgv($, state, kind), { timeoutMs: 10000 })
    if (ran.exitCode === 0) agent = true
    else if (NOT_AGENT.test(ran.stdout)) agent = false
  } catch {
    // The next event reports again.
  }
}

// Queued and not awaited: no turn or tool call waits for a report.
function report($: EngineInterface, state: State, kind?: string) {
  queue = queue.then(() => send($, state, kind)).catch(() => {})
}

const SESSION_ID = /^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/
const SOURCE = /^[a-z]{1,32}$/

// Per module load: the session reports in the order their SessionStarts came (r2b call 25), apart from the activity
// queue, so an earlier session's report never lands after a later one's.
let sessions: Promise<void> = Promise.resolve()

// report-2b K3: this client's session, from classic.SessionStart, where a managed client can run (inMux: an hc
// terminal, tmux or rmux). Queued after the earlier session reports, never awaited, not behind the activity reports;
// every failure is dropped.
export function reportSession($: EngineInterface, sessionId: unknown, source: unknown) {
  if (typeof sessionId !== 'string' || !SESSION_ID.test(sessionId)) return
  const word = typeof source === 'string' && SOURCE.test(source) ? source : 'unknown'
  sessions = sessions.then(async () => {
    if (!(await inMux($))) return
    await $.process.run(await entryArgv($, ['report', '-Session', sessionId, '-Source', word, '-Quiet']), { timeoutMs: 30000 })
  }).catch(() => {})
}

// The main loop only: a subagent's turns and tools (agentId) are not this agent's own.
export const activity = (on: On) => {
  on('turn.start', async ($, e, next) => {
    if (e.agentId === undefined) { report($, 'working'); turnStarted() }
    return next(e)
  })
  on('turn.complete', async ($, e, next) => {
    if (e.agentId === undefined) { report($, 'idle'); turnCompleted() }
    return next(e)
  })
  // A question to the person blocks until it is answered, then the turn works on.
  on('tool.call', { tool: 'AskUserQuestion' }, async ($, e, next) => {
    if (e.agentId !== undefined) return next(e)
    report($, 'blocked', 'question')
    try {
      return await next(e)
    } finally {
      report($, 'working')
    }
  })
}
