import { expect, mock, test } from "claude-code/testing"
import type { Plugin } from "claude-code/testing"
import type { EngineInterface, PromptSubmitInput, PromptSubmitResult, Register, Tier, TraceEntry } from "claude-code"
import { accepted, argv, onSubmit, reset, state, wakeText, type SubmitNext } from "../../hooks/deliver"

type On = Parameters<Register>[0]
type Reply = Record<string, unknown>
const BATCH: Reply = { ok: true, batch: "b1", ids: ["m1"], text: "[mux-agent from a (%5) #m1]\nhi\n" }
const COUNT = (wake: number, end: number, extra: Reply = {}): Reply =>
  ({ ok: true, badge: wake, wake, end, id: "a1", gen: "0", leader: false, member: true, ...extra })
const flagOf = (a: string[]) => a.find(x => x === "-Count" || x === "-Deliver" || x === "-Ack") ?? ""

// ---- Unit: the pure pieces and the interactive path over a fake engine with a synthesized engine trace. The kit
// cannot produce an accepted trace (its answering test hook is plugin "test", tier "builtin", never "engine"/"core").
function fake(replies: Record<string, Reply[]>, env: Record<string, string> = { TMUX: "x" }) {
  const runs: string[][] = []
  const take = (k: string): Reply => { const l = replies[k] ?? [COUNT(0, 0)]; return l.length > 1 ? l.shift()! : l[0] }
  const engine = { plugin: { root: "/p" }, env: { get: async (n: string) => env[n] },
    process: { run: async (a: string[]) => { runs.push(a); return { exitCode: 0, stdout: JSON.stringify(take(flagOf(a))) + "\n" } } } }
  const acks = () => runs.filter(a => a.includes("-Ack")).map(a => a[a.indexOf("-Ack") + 1])
  return { e: engine as unknown as EngineInterface, runs, acks }
}
function entry(plugin: string, tier: Tier, rec: string[], ret: string[] | null, drop = false): TraceEntry<"prompt.submit"> {
  const returned = ret === null ? undefined : drop ? { drop: "no" } : { text: "go", context: ret }
  return { index: 1, plugin, tier, event: "prompt.submit", outcome: ret === null ? "rejected" : "returned", ms: 1,
    received: { text: "go", context: rec } as unknown as PromptSubmitInput, returned: returned as unknown as PromptSubmitResult | undefined }
}
/** A next that records the block it was given and reports @trace for it. */
function nextWith(trace: (block: string) => TraceEntry<"prompt.submit">[], attached: string[] = []): SubmitNext {
  let seen = ""
  const call = async (e: PromptSubmitInput) => { seen = e.context?.at(-1) ?? ""; attached.push(seen); return { text: e.text } as unknown as PromptSubmitResult }
  return Object.defineProperty(call, "trace", { get: () => trace(seen) }) as unknown as SubmitNext
}
const E = { text: "go" } as unknown as PromptSubmitInput
const ENGINE = (b: string) => [entry("engine", "core", [b], [b])]
const WINDOWS_ENTRY = "/p" + String.fromCharCode(92) + "scripts" + String.fromCharCode(92) + "mux-agent.cmd"

test("the entry argv is exact on Windows and elsewhere", async () => {
  expect(await argv(fake({}, { OS: "Windows_NT" }).e, ["read", "-Count"])).toEqual(["cmd.exe", "/d", "/c", WINDOWS_ENTRY, "read", "-Count"])
  expect(await argv(fake({}, {}).e, ["read", "-Count"])).toEqual(["sh", "/p/scripts/mux-agent", "read", "-Count"]) })

test("acceptance is the engine entry receiving and returning the block, without a drop", () => {
  expect(accepted(ENGINE("b"), "b")).toBe(true)
  expect(accepted([entry("echo", "append", ["b"], ["b"])], "b")).toBe(false)
  expect(accepted([entry("engine", "core", [], ["b"])], "b")).toBe(false)
  expect(accepted([entry("engine", "core", ["b"], ["b"], true)], "b")).toBe(false)
  expect(accepted([entry("engine", "core", ["b"], null)], "b")).toBe(false) })

test("an accepted block whose ack fails stays open, is retried first and never re-attached", async () => {
  const cases: [Reply, Reply, boolean][] = [
    [{ ok: false, check: "mail", message: "the acknowledgement could not be saved; retry" }, { ok: true, acked: true }, true],
    [{ ok: false, check: "mail", message: "the last acknowledgement is not yet on disk; retry" }, { ok: true, acked: false, duplicate: true }, true],
    [{ ok: false, check: "mail" }, { ok: false, check: "ack", message: "stale receipt" }, true],
    [{ ok: false, check: "mail" }, { ok: false, check: "mail" }, false]]
  for (const [first, last, retired] of cases) {
    reset()
    const f = fake({ "-Count": [COUNT(0, 0)], "-Deliver": [BATCH, { ok: true, batch: null, ids: [], text: "" }], "-Ack": [first, last] })
    await onSubmit(f.e, E, nextWith(ENGINE))
    expect(state.open.has("b1")).toBe(true)
    let attached: readonly string[] | undefined = ["unset"]
    const bare = Object.assign(async (e: PromptSubmitInput) => { attached = e.context; return { text: "go" } }, { trace: [] })
    await onSubmit(f.e, E, bare as unknown as SubmitNext)
    expect([f.acks(), state.open.has("b1")]).toEqual([["b1", "b1"], !retired])
    if (!retired) expect(attached).toBeUndefined() } })

test("a block the engine did not accept (dropped, or stripped and restored) is attached again and acked once on the accepted retry", async () => {
  const refused: [string, (b: string) => TraceEntry<"prompt.submit">[]][] = [
    ["dropped", b => [entry("engine", "core", [b], [b], true)]], ["stripped and restored", b => [entry("engine", "core", [], [b])]]]
  for (const [what, first] of refused) {
    reset()
    const f = fake({ "-Count": [COUNT(0, 0)], "-Deliver": [BATCH], "-Ack": [{ ok: true, acked: true }] })
    const attached: string[] = []
    await onSubmit(f.e, E, nextWith(first, attached))
    expect([what, f.acks()]).toEqual([what, []])
    await onSubmit(f.e, E, nextWith(ENGINE, attached))
    expect([what, attached.length, attached[0] === attached[1], attached[0].includes("batch=\"b1\""), f.acks()]).toEqual([what, 2, true, true, ["b1"]]) } })

test("a tagged -Ack of an accepted batch cancels the timer and ends delivery", async () => {
  reset()
  let cancelled = 0
  state.cancel = () => { cancelled++ }
  const f = fake({ "-Count": [COUNT(0, 0)], "-Deliver": [BATCH], "-Ack": [{ ok: false, check: "tagged" }] })
  await onSubmit(f.e, E, nextWith(ENGINE))
  const before = f.runs.length
  await onSubmit(f.e, E, nextWith(ENGINE))
  expect([cancelled, state.stopped, f.runs.length - before, f.acks()]).toEqual([1, true, 0, ["b1"]]) })

// ---- The whole plugin through the engine kit: it loads hooks/register.tsx, as the engine does. Inline plugins share
// no memory with the test, so they report through process.run (argv[0] "ECHOED" / "STRIPPED").
type Deliver = () => Reply | "fail" | Promise<Reply | "fail">
function world(on: On, env: Record<string, string>, count: Deliver, deliver: Deliver = () => BATCH) {
  const runs: string[][] = []; const subs: { text: string; context?: string[] }[] = []; let sent = 0
  on("env.get", (_: unknown, e: { name: string }) => ({ value: env[e.name] }))
  on("session.surfaces", () => ({ value: ["terminal"] }))
  on("command.register", () => ({ value: { command: "mux-agents-pane" } }))
  on("session.start", (_: unknown, e: { cwd: string }) => ({ cwd: e.cwd }))
  on("session.send", () => { sent++; return {} })
  on("turn.start", (_: unknown, e: { turnId: string }) => ({ turnId: e.turnId }))
  on("turn.complete", () => ({ text: "" }))
  on("prompt.submit", (_: unknown, e: { text: string; context?: string[] }) => { subs.push({ ...e }); return { text: e.text, context: e.context } })
  on("process.run", async (_: unknown, e: { argv: string[] }) => {
    runs.push([...e.argv])
    const f = flagOf(e.argv)
    const r = f === "-Count" ? await count() : f === "-Deliver" ? await deliver() : { ok: true }
    if (r === "fail") return { value: { exitCode: 1, stdout: "", stderr: "spawn failed", isStdoutTruncated: false, isStderrTruncated: false } }
    return { value: { exitCode: 0, stdout: JSON.stringify(r) + "\n", stderr: "", isStdoutTruncated: false, isStderrTruncated: false } } })
  const n = (f: string) => runs.filter(a => a.includes(f)).length
  const mail = (s?: { context?: string[] }) => (s?.context ?? []).some(c => c.startsWith("<mux-agents-mail"))
  return { runs, n, subs, mail, sent: () => sent, wakes: () => subs.filter(s => s.text.startsWith("mux-agents mail is waiting")).map(s => s.text),
    echoed: () => runs.filter(a => a[0] === "ECHOED" && WAKE.test(a[1])).length, stripped: () => runs.filter(a => a[0] === "STRIPPED").map(a => a[1]) }
}
const START = { cwd: "/w", surface: "terminal", isInteractive: true } as const
const end = (turnId: string) => ({ turnId, reason: "answer", answer: "", durationMs: 1, isAborted: false }) as never
const WAKE = /^mux-agents mail is waiting \(wake [0-9a-f]{8}\): run mux-agent read$/
const ECHO: Plugin = { name: "echo", tier: "append", register(on) {
  on("prompt.submit", async ($, e) => { await $.process.run(["ECHOED", e.text]); return { text: e.text, context: e.context } }) } }
const DROP: Plugin = { name: "drop", tier: "append", register(on) { on("prompt.submit", async () => ({ drop: "no" })) } }
const STRIP: Plugin = { name: "strip", tier: "append", register(on) {
  on("prompt.submit", async ($, e, next) => {
    const block = (e.context ?? []).filter(c => c.startsWith("<mux-agents-mail"))
    for (const b of block) await $.process.run(["STRIPPED", b])
    const r = (await next({ ...e, context: (e.context ?? []).filter(c => !block.includes(c)) })) as { context?: string[] }
    return { ...r, context: [...(r.context ?? []), ...block] } }) } }
const REFUSE_WAKE: Plugin = { name: "refuse", tier: "append", register(on) {
  on("prompt.submit", async (_, e, next) => ((e as { origin?: { kind?: string } }).origin?.kind === "plugin" && e.text.startsWith("mux-agents mail is waiting") ? { drop: "no" } : next(e))) } }

test("an idle member is woken with the fixed text and a nonce; the tick never delivers and never acks", async ($, on) => {
  const clock = mock.clock(on); const w = world(on, { TMUX: "x" }, () => COUNT(1, 10))
  await $.session.start(START)
  await clock.advance(5000)
  expect(w.wakes().length).toBe(1)
  expect(w.wakes()[0]).toMatch(WAKE)
  expect(wakeText("0123abcd")).toBe("mux-agents mail is waiting (wake 0123abcd): run mux-agent read")
  expect([w.n("-Deliver"), w.n("-Ack")]).toEqual([0, 0]) })

test("a wake echoed beneath without being forwarded: the tick still never delivers or acks", { plugins: [ECHO] }, async ($, on) => {
  const clock = mock.clock(on); const w = world(on, { TMUX: "x" }, () => COUNT(1, 10))
  await $.session.start(START)
  await clock.advance(5000); await clock.advance(70000)
  expect([w.echoed() > 0, w.n("-Deliver"), w.n("-Ack")]).toEqual([true, 0, 0]) })

const EXPIRY: [string, () => () => Reply][] = [["an unchanged end", () => () => COUNT(1, 10)],
  ["a changed end", () => { let e = 10; return () => COUNT(1, e++) }]]
for (const [what, make] of EXPIRY) {
  test("an unforwarded wake expires after REFILL_MS; " + what + " re-wakes once then, not before", { plugins: [ECHO] }, async ($, on) => {
    const clock = mock.clock(on); const w = world(on, { TMUX: "x" }, make())
    await $.session.start(START)
    await clock.advance(5000)
    expect(w.echoed()).toBe(1)
    await clock.advance(55000)
    expect(w.echoed()).toBe(1)
    await clock.advance(5000)
    expect(w.echoed()).toBe(2)
    await clock.advance(55000)
    expect(w.echoed()).toBe(2) })
}

test("a busy turn suppresses the wake; it comes after turn.complete", async ($, on) => {
  const clock = mock.clock(on); const w = world(on, { TMUX: "x" }, () => COUNT(1, 10))
  await $.session.start(START)
  await $.turn.start({ text: "go", turnId: "t1" })
  await clock.advance(5000)
  expect(w.wakes().length).toBe(0)
  await $.turn.complete(end("t1"))
  await clock.advance(5000)
  expect(w.wakes().length).toBe(1) })

test("the tick is single-flight while a read -Count still runs", async ($, on) => {
  const clock = mock.clock(on)
  const gate = { open: false }
  let first = true
  const w = world(on, { TMUX: "x" }, async () => {
    if (first) { first = false; while (!gate.open) await clock.sleep(1000) }
    return COUNT(1, 10) })
  await $.session.start(START)
  await clock.advance(5000); await clock.advance(5000); await clock.advance(5000)
  expect(w.n("-Count")).toBe(1)
  gate.open = true
  await clock.advance(1000)
  expect(w.wakes().length).toBe(1) })

test("an unchanged end re-wakes once per refill interval", async ($, on) => {
  const clock = mock.clock(on); const w = world(on, { TMUX: "x" }, () => COUNT(1, 10))
  await $.session.start(START)
  await clock.advance(5000)
  await $.turn.start({ text: "w", turnId: "t1" }); await $.turn.complete(end("t1"))
  await clock.advance(30000)
  expect(w.wakes().length).toBe(1)
  await clock.advance(35000)
  expect(w.wakes().length).toBe(2) })

/** A typed prompt: its -Deliver count, whether the test hook beneath got the mail block, the acks. */
async function typed($: EngineInterface, w: ReturnType<typeof world>) {
  const before = w.n("-Deliver")
  await $.prompt.submit({ text: "typed" })
  return [w.n("-Deliver") - before, w.mail(w.subs.at(-1)), w.n("-Ack")]
}

test("MUX_AGENTS_WAKE=0 arms no timer; a typed prompt still gets exactly one -Deliver", async ($, on) => {
  const clock = mock.clock(on); const w = world(on, { TMUX: "x", MUX_AGENTS_WAKE: "0" }, () => COUNT(1, 10))
  await $.session.start(START)
  await clock.advance(20000)
  expect([w.n("-Count"), w.wakes().length]).toEqual([0, 0])
  expect(await typed($, w)).toEqual([1, true, 0]) })

test("the leader stops only the wake; a typed prompt still gets exactly one -Deliver", async ($, on) => {
  const clock = mock.clock(on); const w = world(on, { TMUX: "x" }, () => COUNT(1, 10, { leader: true }))
  await $.session.start(START)
  await clock.advance(5000); await clock.advance(20000)
  expect([w.n("-Count"), w.wakes().length]).toEqual([1, 0])
  expect(await typed($, w)).toEqual([1, true, 0]) })

test("a refused wake stops only the wake; a typed prompt still gets exactly one -Deliver", { plugins: [REFUSE_WAKE] }, async ($, on) => {
  const clock = mock.clock(on); const w = world(on, { TMUX: "x" }, () => COUNT(1, 10))
  await $.session.start(START)
  await clock.advance(5000); await clock.advance(70000)
  expect(w.n("-Count")).toBe(1)
  expect(await typed($, w)).toEqual([1, true, 0]) })

test("MUX_AGENTS_WAKE=0 set mid-session stops only the wake; a typed prompt still gets exactly one -Deliver", async ($, on) => {
  const clock = mock.clock(on); const env: Record<string, string> = { TMUX: "x" }; const w = world(on, env, () => COUNT(1, 10))
  await $.session.start(START)
  await clock.advance(5000)
  expect(w.wakes().length).toBe(1)
  env.MUX_AGENTS_WAKE = "0"
  await clock.advance(5000); await clock.advance(20000)
  expect([w.n("-Count"), w.wakes().length]).toEqual([2, 1])
  expect(await typed($, w)).toEqual([1, true, 0]) })

const COUNT_FAILS: [string, Reply | "fail"][] = [["a refused -Count", { ok: false, check: "mail", message: "retry" }],
  ["a failed -Count process", "fail"]]
for (const [what, bad] of COUNT_FAILS) {
  test(what + " in the tick stops only the wake; a typed prompt still gets exactly one -Deliver", async ($, on) => {
    const clock = mock.clock(on); let first = true
    const w = world(on, { TMUX: "x" }, () => { if (first) { first = false; return bad } return COUNT(1, 10) })
    await $.session.start(START)
    await clock.advance(5000); await clock.advance(20000)
    expect([w.n("-Count"), w.wakes().length]).toEqual([1, 0])
    expect(await typed($, w)).toEqual([1, true, 0]) })
}

const STOPS: [string, Reply][] = [["a non-member", COUNT(1, 10, { member: false })], ["tagged", { ok: false, check: "tagged" }]]
for (const [what, count] of STOPS) {
  test(what + " is never woken, its timer stops and delivery ends", async ($, on) => {
    const clock = mock.clock(on); const w = world(on, { TMUX: "x" }, () => count)
    await $.session.start(START)
    await clock.advance(5000); await clock.advance(5000)
    expect([w.n("-Count"), w.wakes().length]).toEqual([1, 0])
    expect(await typed($, w)).toEqual([0, false, 0]) })
}

test("an owner change stops the timer and ends delivery", async ($, on) => {
  const clock = mock.clock(on)
  const counts = [COUNT(0, 1), COUNT(0, 1, { gen: "1" })]
  const w = world(on, { TMUX: "x" }, () => (counts.length > 1 ? counts.shift() ?? counts[0] : counts[0]))
  await $.session.start(START)
  await clock.advance(5000); await clock.advance(5000); await clock.advance(5000)
  expect(w.n("-Count")).toBe(2)
  expect(await typed($, w)).toEqual([0, false, 0]) })

test("held-only mail (badge > 0, wake 0) wakes no one; new mail behind held references does", async ($, on) => {
  const clock = mock.clock(on); let c = COUNT(0, 10, { badge: 2 }); const w = world(on, { TMUX: "x" }, () => c)
  await $.session.start(START)
  await clock.advance(5000); await clock.advance(5000)
  expect([w.n("-Count"), w.wakes().length]).toEqual([2, 0])
  c = COUNT(1, 11, { badge: 3 })
  await clock.advance(5000)
  expect(w.wakes().length).toBe(1) })

test("an interactive block echoed beneath without forwarding is never acked and is served again", { plugins: [ECHO] }, async ($, on) => {
  const w = world(on, { TMUX: "x", MUX_AGENTS_WAKE: "0" }, () => COUNT(0, 0))
  await $.prompt.submit({ text: "go" }); await $.prompt.submit({ text: "again" })
  expect([w.n("-Deliver"), w.n("-Ack")]).toEqual([2, 0]) })

test("an interactive block dropped beneath is never acked", { plugins: [DROP] }, async ($, on) => {
  const w = world(on, { TMUX: "x", MUX_AGENTS_WAKE: "0" }, () => COUNT(0, 0))
  await $.prompt.submit({ text: "go" }).catch(() => undefined)
  expect([w.n("-Deliver"), w.n("-Ack")]).toEqual([1, 0]) })

test("an interactive block stripped beneath and restored is never acked and is attached again", { plugins: [STRIP] }, async ($, on) => {
  const w = world(on, { TMUX: "x", MUX_AGENTS_WAKE: "0" }, () => COUNT(0, 0))
  await $.prompt.submit({ text: "go" }); await $.prompt.submit({ text: "again" })
  const s = w.stripped()
  expect([w.n("-Deliver"), w.n("-Ack"), s.length, s[0] === s[1], s[0]?.includes("b1"), w.mail(w.subs.at(-1))])
    .toEqual([2, 0, 2, true, true, false]) })

const FAILED: [string, Deliver][] = [["a refused -Deliver", () => ({ ok: false, check: "mail", message: "retry" })], ["a failed -Deliver process", () => "fail"]]
for (const [what, deliver] of FAILED) {
  test(what + " lets the prompt through with no mail block and no ack", async ($, on) => {
    const w = world(on, { TMUX: "x", MUX_AGENTS_WAKE: "0" }, () => COUNT(0, 0), deliver)
    await $.prompt.submit({ text: "go" })
    expect([w.n("-Count"), w.n("-Deliver"), w.n("-Ack"), w.subs.at(-1)?.text, w.mail(w.subs.at(-1))]).toEqual([1, 1, 0, "go", false]) })
}

test("a tagged -Deliver cancels the timer and ends delivery", async ($, on) => {
  const clock = mock.clock(on); const w = world(on, { TMUX: "x" }, () => COUNT(0, 0), () => ({ ok: false, check: "tagged" }))
  await $.session.start(START)
  await $.prompt.submit({ text: "go" })
  await clock.advance(20000)
  await $.prompt.submit({ text: "again" })
  expect([w.n("-Count"), w.n("-Deliver"), w.n("-Ack"), w.mail(w.subs.at(-1))]).toEqual([1, 1, 0, false]) })

test("an owner change while -Deliver is in flight: the batch is not attached, nothing is acked, delivery ends", async ($, on) => {
  const clock = mock.clock(on); const box = { gen: "0", started: false, open: false }
  const w = world(on, { TMUX: "x", MUX_AGENTS_WAKE: "0" }, () => COUNT(0, 0, { gen: box.gen }), async () => {
    box.started = true
    while (!box.open) await clock.sleep(1000)
    return BATCH })
  const p = $.prompt.submit({ text: "go" })
  for (let i = 0; i < 50 && !box.started; i++) await clock.advance(100)
  expect(box.started).toBe(true)
  box.gen = "1"; box.open = true
  await clock.advance(1000); await p
  await $.prompt.submit({ text: "again" })
  expect([w.n("-Deliver"), w.n("-Ack"), w.subs.length, w.mail(w.subs[0]), w.mail(w.subs[1])]).toEqual([1, 0, 2, false, false]) })

test("a non-member in a person multiplexer gets no delivery", async ($, on) => {
  const w = world(on, { TMUX: "x", MUX_AGENTS_WAKE: "0" }, () => COUNT(1, 10, { member: false }))
  await $.prompt.submit({ text: "go" }); await $.prompt.submit({ text: "again" })
  expect([w.n("-Count"), w.n("-Deliver"), w.n("-Ack")]).toEqual([1, 0, 0]) })

test("no sender wakes anyone and nothing types", async ($, on) => {
  const w = world(on, { TMUX: "x", MUX_AGENTS_WAKE: "0" }, () => COUNT(0, 0))
  const reply = JSON.stringify({ ok: true, verb: "send", from: "a", toSessionId: "11111111-2222-4333-8444-555555555555" })
  on("tool.call", () => ({ result: { stdout: reply } }))
  await $.tool.call({ tool: "Bash", command: "sh /p/scripts/mux-agent send b -BodyFile b.md" } as never)
  expect(w.sent()).toBe(0)
  expect(w.runs.some(a => a.includes("send-keys") || (a[0] === "hc" && a[1] === "send"))).toBe(false) })
