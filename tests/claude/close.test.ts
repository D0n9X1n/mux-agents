import { expect, test } from 'claude-code/testing'

import { register } from '../../hooks/register'

// The 2.1.310 kit's `$` has no `ui.close`, and a test hook's `$.ui.close` is refused (host rule: the scan lists no call),
// so these tests load the real hooks and drive them with a small engine: `$.clock.every` timers fired by `tick`, a
// list that always answers, and `$.state` with versions as `$.state.get` and `$.state.set` answer them.
const LIST = JSON.stringify({ ok: true, verb: 'list', running: 0, agents: [] })

function engine() {
  const hooks: Record<string, { matcher?: Record<string, unknown>; fn: Function }> = {}
  register(((name: string, a: unknown, b?: unknown) => {
    hooks[name] = typeof a === 'function' ? { fn: a } : { matcher: a as Record<string, unknown>, fn: b as Function }
  }) as never)
  const timers: { fn: () => void; cancelled: boolean }[] = []
  const closes: string[] = []
  const state = { value: undefined as unknown, version: 0 }
  let runs = 0
  const $: any = {
    plugin: { root: '/p' },
    ui: { open: async () => ({ isPlaced: true }), status: () => undefined },
    clock: { every: (_ms: number, fn: () => void) => { const t = { fn, cancelled: false }; timers.push(t); return { cancel: () => { t.cancelled = true } } } },
    process: { run: async () => { runs++; return { exitCode: 0, stdout: LIST, stderr: '', isStdoutTruncated: false, isStderrTruncated: false } } },
    state: {
      get: async () => ({ value: state.value, version: state.version }),
      set: async (_ref: unknown, value: unknown, options?: { ifVersion?: number }) => {
        if (options?.ifVersion !== undefined && options.ifVersion !== state.version) return { isSet: false, version: state.version }
        state.value = value; state.version++
        return { isSet: true, version: state.version }
      },
    },
  }
  const flush = async () => { for (let i = 0; i < 20; i++) await new Promise(r => setTimeout(r, 0)) }
  return {
    get runs() { return runs },
    live: () => timers.filter(t => !t.cancelled).length,
    closes,
    open: async () => { await hooks['command.run'].fn($, { command: 'mux-agents-pane' }, async (e: unknown) => e); await flush() },
    // The person closes a pane: the plugin's ui.close hook runs, and its next(e) is the engine closing it.
    close: async (id: string) => {
      const e = { id, origin: { kind: 'person' } }
      const next = async (passed: { id: string }) => { closes.push(passed.id) }
      const hook = hooks['ui.close']
      if (hook && (hook.matcher?.id === undefined || hook.matcher.id === id)) await hook.fn($, e, next)
      else await next(e)
      await flush()
    },
    tick: async () => { for (const t of timers.filter(t => !t.cancelled)) t.fn(); await flush() },
  }
}

test('closing the Agents pane stops its refresh, and the pane still closes', async () => {
  const w = engine()
  await w.open()
  expect([w.runs, w.live()]).toEqual([1, 1])
  await w.close('mux-agents')
  for (let i = 0; i < 3; i++) await w.tick()
  expect([w.runs, w.live(), w.closes]).toEqual([1, 0, ['mux-agents']])
})

test('reopening the pane after a close starts exactly one new timer', async () => {
  const w = engine()
  await w.open()
  await w.close('mux-agents')
  await w.open()
  expect([w.runs, w.live()]).toEqual([2, 1])
  await w.tick()
  await w.open()
  await w.tick()
  expect([w.runs, w.live()]).toEqual([4, 1])
  await w.close('mux-agents')
})

test('closing another pane leaves the refresh running', async () => {
  const w = engine()
  await w.open()
  await w.close('other')
  await w.tick()
  expect([w.runs, w.live(), w.closes]).toEqual([2, 1, ['other']])
  await w.close('mux-agents')
})
