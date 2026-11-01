import { atom, read, update } from 'claude-code'
import type { EngineInterface, Register, Timer } from 'claude-code'

import { EMPTY, formatAge, isSpawn, parseList } from './board'

const PANE = 'mux-agents'
// Change the shape tag when the board's fields change: it declines what an earlier version stored.
const board = atom({ plugin: 'mux-agents', key: 'board' } as const, EMPTY, { shape: 'board-2' })
// Per module load (a hot reload drops the timer and resets both): the one refresh timer while the pane is open, and at
// most one list run at a time.
let ticking: Timer | undefined
let busy = false
// Bumped by every retire and by a client that is not the terminal attaching: an open begun before the bump is no longer
// current, so it starts no timer and no list and closes the pane it opened.
let epoch = 0

const NOT_HERE = 'mux-agents works only in a terminal CLI inside tmux or rmux.'

// Terminal CLI only (#43), read fresh each time and never cached: the session draws on the terminal and nothing else
// ($.ui.open has no surface selector, so with a desktop, mobile or IDE client attached too the pane would show there as
// an empty tab), it runs inside tmux or rmux, and it is not a cloud session. A refused read, or no surface at all
// (a headless or SDK run), is not eligible. The surfaces, the reading most likely to change, are read last.
async function eligible($: EngineInterface): Promise<boolean> {
  try {
    if ((await $.env.get('CLAUDE_CODE_REMOTE')) === 'true') return false
    if (!((await $.env.get('TMUX')) || (await $.env.get('RMUX')))) return false
    const surfaces = await $.session.surfaces()
    return surfaces.length === 1 && surfaces[0] === 'terminal'
  } catch {
    return false
  }
}

// Stops the refresh at once, then asks the pane to close. The close is best effort: a hook may refuse it or answer
// without closing, and that pane keeps its last board while nothing polls.
async function retire($: EngineInterface) {
  epoch++
  ticking?.cancel()
  ticking = undefined
  try {
    await $.ui.close({ id: PANE })
  } catch {
    // Kept open below; the refresh is already stopped.
  }
}

// The entry shim's `list -All`. Hooks have no `process`, so the OS comes from the environment; an unreadable OS is
// taken as not Windows.
async function listArgv($: EngineInterface): Promise<string[]> {
  const root = $.plugin.root
  let os
  try { os = await $.env.get('OS') } catch { os = undefined }
  if (os === 'Windows_NT') return ['cmd.exe', '/d', '/c', `${root}\\scripts\\mux-agent.cmd`, 'list', '-All']
  return ['sh', `${root}/scripts/mux-agent`, 'list', '-All']
}

// One list run for `timer`. Eligibility is checked first on every tick, even while the last list still runs, and right
// before list runs; a timer already cancelled runs nothing. `busy` only keeps a second list from overlapping.
async function refresh($: EngineInterface, argv: string[], timer: Timer) {
  const ok = await eligible($)
  if (ticking !== timer) return
  if (!ok) {
    await retire($)
    return
  }
  if (busy) return
  busy = true
  try {
    let next
    try {
      const ran = await $.process.run(argv, { timeoutMs: 10000 })
      next = parseList(ran.exitCode, ran.stdout)
    } catch {
      next = undefined
    }
    if (!next) {
      await update($, board, last => ({ ...last, failed: true }))
      return
    }
    const fresh = next
    await update($, board, () => ({ ...fresh, failed: false }))
    $.ui.status(`agents: ${fresh.running}`)
  } finally {
    busy = false
  }
}

// A refresh started and not awaited: a rejection (a failed state write) is dropped, and the next tick tries again.
function startRefresh($: EngineInterface, argv: string[], timer: Timer) {
  refresh($, argv, timer).catch(() => {})
}

// Opens the pane; the first refresh runs on its own, so neither /mux-agents-pane nor a tool result waits for list.
// Eligibility may be lost while the open waits, so it is checked after every other wait of the open: when it is lost,
// or the open is no longer current, no timer starts and the pane just opened is closed. Each refresh checks again
// before list runs. Answers whether the pane is open and refreshing.
async function openPane($: EngineInterface): Promise<boolean> {
  const since = epoch
  await $.ui.open({ id: PANE, title: 'Agents' })
  const argv = await listArgv($)
  const ok = await eligible($)
  if (since !== epoch || !ok) {
    await retire($)
    return false
  }
  if (!ticking) {
    const timer: Timer = $.clock.every(3000, () => startRefresh($, argv, timer))
    ticking = timer
    startRefresh($, argv, timer)
  }
  return true
}

// After a Bash or PowerShell call: opens the pane when the call was a successful spawn in an eligible session.
async function openAfterSpawn($: EngineInterface, command: string, stdout: string | undefined) {
  if (isSpawn(command, stdout) && (await eligible($))) await openPane($)
}

// After a close of the pane: stop its refresh only when the pane is really gone, so a later open starts one new timer.
// A close refused below, or answered without closing, leaves the pane listed and its refresh running. When the list
// cannot be read the refresh runs on: a spare list every 3 s costs less than a pane that stops updating.
async function stopRefreshIfGone($: EngineInterface) {
  let panes
  try {
    panes = await $.ui.panes()
  } catch {
    return
  }
  if (panes.some(p => p.id === PANE)) return
  ticking?.cancel()
  ticking = undefined
}

export const register: Register = on => {
  // The skill skills/mux-agents owns /mux-agents, so the pane command has a name of its own, registered only in an
  // eligible session. A refused registration (another owner of the name) leaves the session and the open-on-spawn
  // path working.
  on('session.start', async ($, e, next) => {
    try {
      if (await eligible($)) await $.command.register({ name: 'mux-agents-pane', description: 'Show the mux-agents board in a pane' })
    } catch {
      // The pane still opens after a spawn; only the command is missing.
    }
    return next(e)
  })
  // Registered while eligible does not mean eligible now: the command checks again.
  on('command.run', { command: 'mux-agents-pane' }, async $ => {
    if (!(await eligible($)) || !(await openPane($))) return { text: NOT_HERE }
    return { text: 'Agents pane opened.' }
  })
  // A desktop, mobile or IDE client attaching: an open still pending is no longer current, and with the pane open the
  // refresh stops at once and the pane is asked to close, without holding up the attach.
  on('session.attach', async ($, e, next) => {
    if (e.surface !== 'terminal') {
      epoch++
      if (ticking) retire($).catch(() => {})
    }
    return next(e)
  })
  // The pane never delays or changes the tool result: the spawn check and the open run on their own, and any rejection
  // ($.env.get, $.ui.open) is dropped.
  on('tool.call', async ($, e, next) => {
    const ran = await next(e)
    const tool = String(e.tool)
    if ((tool === 'Bash' || tool === 'PowerShell') && ran.deny === undefined && ran.isError !== true) {
      const stdout = (ran.result as { stdout?: unknown } | undefined)?.stdout
      openAfterSpawn($, String((e as { command?: unknown }).command ?? ''), typeof stdout === 'string' ? stdout : undefined).catch(() => {})
    }
    return ran
  })
  // Any origin (the person, a plugin): the close goes on first; its answer is returned unchanged.
  on('ui.close', async ($, e, next) => {
    const closed = await next(e)
    if (e.id === PANE) await stopRefreshIfGone($)
    return closed
  })
  // Every string drawn here comes from parseList, so it is already cleaned. The terminal only: a pane kept open
  // elsewhere draws nothing.
  on('ui.render', { component: 'Pane', requestId: PANE, surface: 'terminal' }, async ($, e) => {
    const { Box, Text } = $.ui.resolve(e)
    const b = await read($, board)
    return (
      <Box flexDirection="column">
        <Text>mux-agents · {b.running} running{b.failed ? ' · refresh failed' : ''}</Text>
        {b.agents.map(a => {
          const ended = a.state !== 'running'
          // A live agent's chosen model follows its client.
          const tag = !ended && a.model ? ` [${a.model}]` : ''
          return (
            <Box flexDirection="column">
              <Text dimColor={ended || undefined}>{ended ? '○' : '●'} {a.name}  {a.client}{tag}  {ended ? `${a.state}  ` : ''}{formatAge(a.age)}</Text>
              {a.doing ? <Text dimColor>  {a.doing}</Text> : null}
            </Box>
          )
        })}
      </Box>
    )
  })
}
