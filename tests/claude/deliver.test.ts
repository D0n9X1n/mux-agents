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
