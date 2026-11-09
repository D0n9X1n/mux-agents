// What this session's own client is doing, reported for the side bar as herdr's agents report theirs (OSC 7501):
// working while a turn runs, blocked while it asks the person, idle once it ends. Only an agent's own pane is
// answered; anywhere else `report` refuses `target` and this session stops asking.
import type { EngineInterface, Register } from 'claude-code'

type On = Parameters<Register>[0]
type State = 'working' | 'blocked' | 'idle'

// Per module load: the reports in the order their events came, one at a time, so a slow one never lands after a
// later one; and whether this session is an agent's (unknown until the first answer).
let queue: Promise<void> = Promise.resolve()
let agent: boolean | undefined

// The entry shim's `report`. Hooks have no `process`, so the OS comes from the environment.
async function reportArgv($: EngineInterface, state: State, kind?: string): Promise<string[]> {
  const root = $.plugin.root
  let os
  try { os = await $.env.get('OS') } catch { os = undefined }
  const tail = kind ? ['report', state, '-Kind', kind] : ['report', state]
  if (os === 'Windows_NT') return ['cmd.exe', '/d', '/c', `${root}\\scripts\\mux-agent.cmd`, ...tail]
  return ['sh', `${root}/scripts/mux-agent`, ...tail]
}

// Inside tmux or rmux at all: a pane that could be an agent's.
async function inMux($: EngineInterface): Promise<boolean> {
  try {
    return Boolean((await $.env.get('TMUX')) || (await $.env.get('RMUX')))
  } catch {
    return false
  }
}

// One report, after every earlier one. A refusal naming the target means this pane is no agent's (or no longer is):
// no more reports.
// Every failure is dropped.
async function send($: EngineInterface, state: State, kind?: string) {
  if (agent === false || !(await inMux($))) return
  try {
    const ran = await $.process.run(await reportArgv($, state, kind), { timeoutMs: 10000 })
    if (ran.exitCode === 0) agent = true
    else if (/"check":"target"/.test(ran.stdout)) agent = false
  } catch {
    // The next event reports again.
  }
}

// Queued and not awaited: no turn or tool call waits for a report.
function report($: EngineInterface, state: State, kind?: string) {
  queue = queue.then(() => send($, state, kind)).catch(() => {})
}

// The main loop only: a subagent's turns and tools (agentId) are not this agent's own.
export const activity = (on: On) => {
  on('turn.start', async ($, e, next) => {
    if (e.agentId === undefined) report($, 'working')
    return next(e)
  })
  on('turn.complete', async ($, e, next) => {
    if (e.agentId === undefined) report($, 'idle')
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
