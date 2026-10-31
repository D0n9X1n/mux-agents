import { atom, read, update } from 'claude-code'
import type { EngineInterface, Register } from 'claude-code'

import { EMPTY, formatAge, isSpawn, parseList } from './board'

const PANE = 'mux-agents'
// Change the shape tag when the board's fields change: it declines what an earlier version stored.
const board = atom({ plugin: 'mux-agents', key: 'board' } as const, EMPTY, { shape: 'board-1' })
// Per module load (a hot reload drops the timer and resets both): one timer, and at most one list run at a time.
let ticking = false
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

async function openPane($: EngineInterface) {
  await $.ui.open({ id: PANE, title: 'Agents' })
  if (!ticking) {
    ticking = true
    $.clock.every(3000, () => refresh($))
    await refresh($)
  }
}

export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    await $.command.register({ name: 'mux-agents', description: 'Show the mux-agents board in a pane' })
    return next(e)
  })
  on('command.run', { command: 'mux-agents' }, async $ => {
    await openPane($)
    return { text: 'Agents pane opened.' }
  })
  on('tool.call', async ($, e, next) => {
    const ran = await next(e)
    const tool = String(e.tool)
    if ((tool === 'Bash' || tool === 'PowerShell') && ran.deny === undefined && ran.isError !== true) {
      const mux = (await $.env.get('TMUX')) || (await $.env.get('RMUX'))
      const stdout = (ran.result as { stdout?: unknown } | undefined)?.stdout
      if (mux && isSpawn(String((e as { command?: unknown }).command ?? ''), typeof stdout === 'string' ? stdout : undefined)) await openPane($)
    }
    return ran
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
