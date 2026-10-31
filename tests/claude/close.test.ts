import { expect, test } from 'claude-code/testing'

import { register } from '../../hooks/register'

// The 2.1.310 kit's `$` has no `ui.close`, and a test hook's `$.ui.close` is refused (host rule: the scan lists no call),
// so these tests load the real hooks and drive them with a small engine: `$.clock.every` timers fired by `tick`, a
// list that always answers, `$.state` with versions as `$.state.get` and `$.state.set` answer them, and `$.ui.panes`
// listing the panes opened and not yet closed.
const LIST = JSON.stringify({ ok: true, verb: 'list', running: 0, agents: [] })

// What answers a close beneath the plugin: the engine closing the pane, a hook refusing it, or a hook answering
// without passing it on, so the pane stays open.
type Below = 'close' | 'deny' | 'keep'

function engine() {
  const hooks: Record<string, { matcher?: Record<string, unknown>; fn: Function }> = {}
  register(((name: string, a: unknown, b?: unknown) => {
    hooks[name] = typeof a === 'function' ? { fn: a } : { matcher: a as Record<string, unknown>, fn: b as Function }
  }) as never)
  const timers: { fn: () => void; cancelled: boolean }[] = []
  const closes: string[] = []
  const answers: unknown[] = []
  const panes = new Set<string>()
  const state = { value: undefined as unknown, version: 0 }
  let runs = 0
  let panesFail = false
  // With `hold`, each list run waits until `release`; `inflight` counts runs started and not yet answered.
  let hold = false
  let inflight = 0
  let maxInflight = 0
  const waiting: (() => void)[] = []
  const answer = { exitCode: 0, stdout: LIST, stderr: '', isStdoutTruncated: false, isStderrTruncated: false }
  const $: any = {
    plugin: { root: '/p' },
    ui: {
      open: async (e: { id: string }) => { panes.add(e.id); return { isPlaced: true } },
      panes: async () => { if (panesFail) throw new Error('ui.panes refused'); return [...panes].map(id => ({ id, title: id })) },
      status: () => undefined,
    },
    clock: { every: (_ms: number, fn: () => void) => { const t = { fn, cancelled: false }; timers.push(t); return { cancel: () => { t.cancelled = true } } } },
    process: {
      run: async () => {
        runs++; inflight++; maxInflight = Math.max(maxInflight, inflight)
        if (hold) await new Promise<void>(resolve => waiting.push(resolve))
        inflight--
        return answer
      },
    },
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
    get maxInflight() { return maxInflight },
    timers: () => timers.length,
    live: () => timers.filter(t => !t.cancelled).length,
    closes,
    answers,
    failPanes: (fail: boolean) => { panesFail = fail },
    holdRuns: (on: boolean) => { hold = on },
    release: async () => { hold = false; for (const resolve of waiting.splice(0)) resolve(); await flush() },
    open: async () => { await hooks['command.run'].fn($, { command: 'mux-agents-pane' }, async (e: unknown) => e); await flush() },
    // A close of pane `id`: the plugin's ui.close hook runs, and its next(e) is what stands beneath it (`below`).
    close: async (id: string, below: Below = 'close') => {
      const e = { id, origin: { kind: 'person' } }
      const next = async (passed: { id: string }) => {
        if (below === 'deny') return { deny: 'keep this pane open' }
        if (below === 'close') { panes.delete(passed.id); closes.push(passed.id) }
        return { value: undefined }
      }
      const hook = hooks['ui.close']
      answers.push(hook && (hook.matcher?.id === undefined || hook.matcher.id === id) ? await hook.fn($, e, next) : await next(e))
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
  expect([w.runs, w.live(), w.closes, w.answers]).toEqual([1, 0, ['mux-agents'], [{ value: undefined }]])
})

test('a close refused below keeps the pane refreshing, and the refusal is returned unchanged', async () => {
  const w = engine()
  await w.open()
  await w.close('mux-agents', 'deny')
  await w.tick()
  expect([w.runs, w.live(), w.closes, w.answers]).toEqual([2, 1, [], [{ deny: 'keep this pane open' }]])
  await w.close('mux-agents')
})

test('a close answered below without closing, the pane still listed, keeps it refreshing', async () => {
  const w = engine()
  await w.open()
  await w.close('mux-agents', 'keep')
  await w.tick()
  expect([w.runs, w.live(), w.closes]).toEqual([2, 1, []])
  await w.close('mux-agents')
})

test('when ui.panes rejects after a close, the refresh keeps running', async () => {
  const w = engine()
  await w.open()
  w.failPanes(true)
  await w.close('mux-agents')
  await w.tick()
  expect([w.runs, w.live(), w.closes]).toEqual([2, 1, ['mux-agents']])
  w.failPanes(false)
  await w.open()
  await w.close('mux-agents')
  expect(w.live()).toBe(0)
})

test('a refresh still running when the pane closes does not restart the timer when it settles', async () => {
  const w = engine()
  w.holdRuns(true)
  await w.open()
  await w.close('mux-agents')
  expect([w.runs, w.timers(), w.live()]).toEqual([1, 1, 0])
  await w.release()
  await w.tick()
  expect([w.runs, w.timers(), w.live()]).toEqual([1, 1, 0])
})

test('reopening before an outstanding refresh settles keeps one timer and never overlaps list runs', async () => {
  const w = engine()
  w.holdRuns(true)
  await w.open()
  await w.close('mux-agents')
  await w.open()
  await w.tick()
  expect([w.runs, w.live(), w.maxInflight]).toEqual([1, 1, 1])
  await w.release()
  await w.tick()
  expect([w.runs, w.live(), w.maxInflight]).toEqual([2, 1, 1])
  await w.close('mux-agents')
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
