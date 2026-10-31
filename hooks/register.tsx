import { atom, read, update } from 'claude-code'
import type { EngineInterface, Register, Timer } from 'claude-code'

import { EMPTY, formatAge, isSpawn, parseList } from './board'

const PANE = 'mux-agents'
// Change the shape tag when the board's fields change: it declines what an earlier version stored.
const board = atom({ plugin: 'mux-agents', key: 'board' } as const, EMPTY, { shape: 'board-1' })
// Per module load (a hot reload drops the timer and resets both): the one refresh timer while the pane is open, and at
// most one list run at a time.
let ticking: Timer | undefined
let busy = false

async function refresh($: EngineInterface) {
  if (busy) return
  busy = true
  try {
    let next
    try {
      const ran = await $.process.run(['pwsh', '-NoProfile', '-File', `${$.plugin.root}/scripts/mux-agent.ps1`, 'list', '-All'], { timeoutMs: 10000 })
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
function startRefresh($: EngineInterface) {
  refresh($).catch(() => {})
}

// Opens the pane; the first refresh runs on its own, so neither /mux-agents-pane nor a tool result waits for list.
async function openPane($: EngineInterface) {
  await $.ui.open({ id: PANE, title: 'Agents' })
  if (!ticking) {
    ticking = $.clock.every(3000, () => startRefresh($))
    startRefresh($)
  }
}

// After a Bash or PowerShell call: opens the pane when the call was a successful spawn inside tmux or rmux.
async function openAfterSpawn($: EngineInterface, command: string, stdout: string | undefined) {
  const mux = (await $.env.get('TMUX')) || (await $.env.get('RMUX'))
  if (mux && isSpawn(command, stdout)) await openPane($)
}

// After the pane closed: stop its refresh, so a later open starts one new timer.
function stopRefresh() {
  ticking?.cancel()
  ticking = undefined
}

export const register: Register = on => {
  // The skill skills/mux-agents owns /mux-agents, so the pane command has a name of its own. A refused registration
  // (another owner of the name) leaves the session and the open-on-spawn path working.
  on('session.start', async ($, e, next) => {
    try {
      await $.command.register({ name: 'mux-agents-pane', description: 'Show the mux-agents board in a pane' })
    } catch {
      // The pane still opens after a spawn; only the command is missing.
    }
    return next(e)
  })
  on('command.run', { command: 'mux-agents-pane' }, async $ => {
    await openPane($)
    return { text: 'Agents pane opened.' }
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
  // Any origin (the person, a plugin): the close goes on first, so a hook below that keeps the pane open keeps its refresh.
  on('ui.close', async ($, e, next) => {
    const closed = await next(e)
    if (e.id === PANE) stopRefresh()
    return closed
  })
  // Every string drawn here comes from parseList, so it is already cleaned.
  on('ui.render', { component: 'Pane', requestId: PANE }, async ($, e) => {
    const { Box, Text } = $.ui.resolve(e)
    const b = await read($, board)
    return (
      <Box flexDirection="column">
        <Text>mux-agents · {b.running} running{b.failed ? ' · refresh failed' : ''}</Text>
        {b.agents.map(a => {
          const ended = a.state !== 'running'
          return (
            <Box flexDirection="column">
              <Text dimColor={ended || undefined}>{ended ? '○' : '●'} {a.name}  {a.client}  {ended ? `${a.state}  ` : ''}{formatAge(a.age)}</Text>
              {a.doing ? <Text dimColor>  {a.doing}</Text> : null}
            </Box>
          )
        })}
      </Box>
    )
  })
}
