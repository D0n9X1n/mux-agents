// Mail at the turn of a Claude member (4b-ii). W2: an interactive prompt.submit attaches one receipt-bearing
// block, acked only when the engine itself received and returned it. W3 (fix cycle 2): an idle member is woken with a
// fixed text and a nonce; the wake never delivers and never acks: the agent reads and acks its mail itself.
// Two stops (fix cycle 3): the wake alone stops for the leader, a refused wake, MUX_AGENTS_WAKE=0 or a failed tick;
// delivery (and the wake with it) ends only on `tagged`, `member:false` or an owner change.
import type { EngineInterface, Next, PromptSubmitInput, TraceEntry } from "claude-code"

export type SubmitNext = Next<"prompt.submit">
type Reply = Record<string, unknown>
/** One re-wake per this many ms while `end` is unchanged; an unanswered wake also expires after it. */
export const REFILL_MS = 60000

// Per module load.
export const state = { open: new Set<string>(), stopped: false, wakeStopped: false, member: undefined as boolean | undefined,
  busy: false, outstandingAt: -Infinity, owner: undefined as string | undefined, wokenEnd: 0, wokenAt: -Infinity,
  ticking: false, cancel: undefined as (() => void) | undefined }
export function reset() {
  state.cancel?.()
  Object.assign(state, { stopped: false, wakeStopped: false, member: undefined, busy: false, outstandingAt: -Infinity,
    owner: undefined, wokenEnd: 0, wokenAt: -Infinity, ticking: false, cancel: undefined })
  state.open.clear()
}
/** The wake alone stops: the timer is cancelled; interactive delivery goes on. */
export function stopWake() { state.wakeStopped = true; state.cancel?.(); state.cancel = undefined }
/** Delivery ends (tagged, not a member, another owner): no more delivery, ack or wake. */
export function stop() { state.stopped = true; stopWake() }

/** W2: the engine entry (plugin engine, tier core) received and returned this exact block, and returned no drop. */
export function accepted(trace: readonly TraceEntry<"prompt.submit">[], block: string): boolean {
  const engine = trace.find(t => t.plugin === "engine" && t.tier === "core")
  const ret = engine?.returned as { context?: readonly string[]; drop?: unknown } | undefined
  if (!engine || !ret || ret.drop !== undefined) return false
  const rec = engine.received as { context?: readonly string[] }
  return Boolean(rec.context?.includes(block) && ret.context?.includes(block))
}

/** The entry shim argv, exactly as hooks/activity.ts:20 builds it. */
export async function argv($: EngineInterface, args: string[]): Promise<string[]> {
  const root = $.plugin.root
  let os
  try { os = await $.env.get("OS") } catch { os = undefined }
  if (os === "Windows_NT") return ["cmd.exe", "/d", "/c", `${root}\\scripts\\mux-agent.cmd`, ...args]
  return ["sh", `${root}/scripts/mux-agent`, ...args]
}

/** One run; its last JSON line, or undefined. A `tagged` answer from any read stops. */
export async function run($: EngineInterface, args: string[]): Promise<Reply | undefined> {
  try {
    const ran = await $.process.run(await argv($, args), { timeoutMs: 10000 })
    const r = JSON.parse(ran.stdout.trim().split("\n").at(-1) ?? "") as Reply
    if (r.check === "tagged") stop()
    return r
  } catch { return undefined }
}

export function mailBlock(batch: string, text: string): string {
  return `<mux-agents-mail batch="${batch}">\n${text}</mux-agents-mail>`
}

async function inTeam($: EngineInterface): Promise<boolean> {
  try { return Boolean((await $.env.get("HC_TERMINAL_ID")) || (await $.env.get("TMUX")) || (await $.env.get("RMUX"))) } catch { return false }
}
async function wakeOff($: EngineInterface): Promise<boolean> {
  try { return (await $.env.get("MUX_AGENTS_WAKE")) === "0" } catch { return false }
}


/** Members only: asks `read -Count` once per load; a non-member (or tagged) delivers and acks nothing, and stops. */
async function eligible($: EngineInterface): Promise<boolean> {
  if (state.stopped) return false
  if (state.member === undefined) {
    const c = await run($, ["read", "-Count"])
    if (state.stopped || !c || c.ok === false) return false
    if (c.member !== true) { stop(); return false }
    if (!sameOwner(c)) return false
    state.member = true
  }
  return !state.stopped
}

/** The first `read -Count` of this load names its owner; another owner later ends delivery and the wake. */
function sameOwner(c: Reply): boolean {
  const owner = `${String(c.id)}/${String(c.gen)}`
  if (state.owner === undefined) state.owner = owner
  if (state.owner === owner) return true
  stop()
  return false
}

/** Retries the ack of `batch`; retired on success, `duplicate:true` or a stale answer (its mail is re-served). */
async function retire($: EngineInterface, batch: string) {
  const r = await run($, ["read", "-Ack", batch])
  if (r && (r.ok !== false || r.check === "ack")) state.open.delete(batch)
}

/** W2, the prompt.submit body of an interactive turn: acks first, then one block, acked only on the engine trace.
 * The owner is read again after `-Deliver`: a batch served across an owner change is never attached (it stays unacked
 * and is rebuilt for the new owner). */
export async function onSubmit($: EngineInterface, e: PromptSubmitInput, next: SubmitNext) {
  if (!(await inTeam($)) || !(await eligible($))) return next(e)
  for (const b of [...state.open]) await retire($, b)
  if (state.stopped || state.open.size > 0) return next(e)
  const r = await run($, ["read", "-Deliver"])
  if (state.stopped || !r || r.ok === false || typeof r.batch !== "string" || typeof r.text !== "string" || r.text === "") return next(e)
  const c = await run($, ["read", "-Count"])
  if (state.stopped || !c || c.ok === false || c.member !== true || !sameOwner(c)) {
    if (c && c.ok !== false && c.member !== true) stop()
    return next(e)
  }
  const block = mailBlock(r.batch, r.text)
  const res = await next({ ...e, context: [...(e.context ?? []), block] })
  if (accepted(next.trace, block)) { state.open.add(r.batch); await retire($, r.batch) }
  return res
}

export function wakeText(nonce: string): string { return `mux-agents mail is waiting (wake ${nonce}): run mux-agent read` }
function nonce(): string { return Math.floor(Math.random() * 0x100000000).toString(16).padStart(8, "0") }

/** W3: a single-flight tick. It never delivers and never acks. It wakes when the member is idle, eligible,
 * `wake > 0`, its owner unchanged and no wake outstanding; an outstanding wake expires after REFILL_MS even if no turn
 * started (an unforwarded wake); an unchanged `end` re-wakes at most once per REFILL_MS. Every condition is checked
 * again after each await, right before the submit. */
export async function tick($: EngineInterface) {
  if (state.ticking || state.stopped || state.wakeStopped) return
  state.ticking = true
  try {
    const c = await run($, ["read", "-Count"])
    if (state.stopped || state.wakeStopped) return
    // A refused or failed read -Count (not tagged: run() already stopped) ends the wake only; W2 goes on.
    if (!c || c.ok === false) return stopWake()
    if (c.member !== true) return stop()
    if (!sameOwner(c)) return
    if (c.leader === true) return stopWake()
    if (await wakeOff($)) return stopWake()
    const owner = state.owner
    const end = Number(c.end)
    if (!(Number(c.wake) > 0)) return
    const now = await $.clock.now()
    if (now - state.outstandingAt < REFILL_MS) return
    if (end <= state.wokenEnd && now - state.wokenAt < REFILL_MS) return
    if (state.stopped || state.wakeStopped || state.busy || state.owner !== owner) return
    if (now - state.outstandingAt < REFILL_MS) return
    state.outstandingAt = now
    state.wokenEnd = end
    state.wokenAt = now
    const res = (await $.prompt.submit({ text: wakeText(nonce()) })) as { drop?: unknown }
    if (res?.drop !== undefined) stopWake()
  } catch { stopWake() } finally { state.ticking = false }
}

/** session.start: arms the 5 s tick once (in a multiplexer or hc terminal; not with MUX_AGENTS_WAKE=0). */
export async function arm($: EngineInterface) {
  if (state.cancel || state.stopped || state.wakeStopped) return
  if ((await wakeOff($)) || !(await inTeam($)) || state.cancel || state.stopped || state.wakeStopped) return
  const timer = $.clock.every(5000, () => { tick($).catch(() => {}) })
  state.cancel = () => timer.cancel()
}
/** turn.start / turn.complete of the main loop (folded into activity.ts). */
export function turnStarted() { state.busy = true; state.outstandingAt = -Infinity }
export function turnCompleted() { state.busy = false }
