import { expect, test } from 'claude-code/testing'

import { clean, formatAge, isSpawn, parseList } from '../../hooks/board'
import {
  SPAWN_ERROR_JSON, SPAWN_ERROR_TEXT, SPAWN_JSON, SPAWN_OFF_JSON, SPAWN_OFF_TEXT, SPAWN_TEXT, TAGGED_JSON, TAGGED_TEXT,
} from './spawn-replies'

const E = 'sh "/p/scripts/mux-agent"'
const LISTED = '{"ok":true,"verb":"list","running":0,"agents":[]}'

test('isSpawn accepts every spawn form with a real spawn reply, JSON or -Text', () => {
  expect(isSpawn(`${E} spawn -Client claude`, SPAWN_JSON)).toBe(true)
  expect(isSpawn("pwsh -File '/p/scripts/mux-agent.ps1' spawn", SPAWN_JSON)).toBe(true)
  expect(isSpawn('pwsh -File /p/scripts/mux-agent.ps1 "spawn" -Client claude', SPAWN_JSON)).toBe(true)
  expect(isSpawn('PWSH -FILE C:/P/SCRIPTS/MUX-AGENT.PS1 SPAWN', SPAWN_JSON)).toBe(true)
  expect(isSpawn('& "C:/p/scripts/mux-agent.ps1" spawn -Client copilot', SPAWN_JSON)).toBe(true)
  expect(isSpawn('pwsh -File mux-agent.ps1 spawn', SPAWN_JSON)).toBe(true)
  expect(isSpawn(`${E} spawn`, `${SPAWN_JSON}\r\n`)).toBe(true)
  expect(isSpawn(`${E} spawn -Text`, SPAWN_TEXT)).toBe(true)
  expect(isSpawn(`${E} spawn -Text`, `${SPAWN_TEXT.replace(/\n/g, '\r\n')}\r\n`)).toBe(true)
})

test('isSpawn accepts a spawn whose side bar is off or failed after the commit, JSON or -Text', () => {
  for (const reply of [SPAWN_OFF_JSON, SPAWN_ERROR_JSON, SPAWN_OFF_TEXT, SPAWN_ERROR_TEXT]) {
    expect(isSpawn(`${E} spawn`, reply)).toBe(true)
    expect(isSpawn(`${E} spawn -Text`, `${reply.replace(/\n/g, '\r\n')}\r\n`)).toBe(true)
  }
})

test('isSpawn accepts a Windows path with backslashes, quoted or not', () => {
  expect(isSpawn('pwsh -NoProfile -File "C:\\p\\scripts\\mux-agent.ps1" "spawn"', SPAWN_JSON)).toBe(true)
  expect(isSpawn('& C:\\p\\scripts\\mux-agent.ps1 spawn -Client claude', SPAWN_JSON)).toBe(true)
})

test('isSpawn accepts mux-agent, mux-agent.cmd and mux-agent.exe spawn', () => {
  expect(isSpawn('"/p/plug in/scripts/mux-agent" spawn -Client claude', SPAWN_JSON)).toBe(true)
  expect(isSpawn('/p/scripts/mux-agent spawn', SPAWN_JSON)).toBe(true)
  expect(isSpawn('sh /p/scripts/mux-agent "spawn"', SPAWN_JSON)).toBe(true)
  expect(isSpawn('& "C:\\p\\scripts\\mux-agent.cmd" spawn -Client copilot', SPAWN_JSON)).toBe(true)
  expect(isSpawn('cmd.exe /d /c C:/p/scripts/MUX-AGENT.CMD spawn', SPAWN_JSON)).toBe(true)
  expect(isSpawn("'C:/p/target/release/mux-agent.exe' spawn", SPAWN_JSON)).toBe(true)
  expect(isSpawn('mux-agent.exe spawn -Text', SPAWN_TEXT)).toBe(true)
})

test('isSpawn still accepts mux-agent.ps1 spawn', () => {
  expect(isSpawn('pwsh -NoProfile -File "C:/p/scripts/mux-agent.ps1" spawn', SPAWN_JSON)).toBe(true)
})

test('isSpawn rejects mux-agents spawn and mux-agent list', () => {
  expect(isSpawn('/p/scripts/mux-agents spawn', SPAWN_JSON)).toBe(false)
  expect(isSpawn('mux-agents spawn', SPAWN_JSON)).toBe(false)
  expect(isSpawn('/p/scripts/mux-agent list', LISTED)).toBe(false)
  expect(isSpawn('/p/scripts/mux-agent.cmd list', SPAWN_JSON)).toBe(false)
  expect(isSpawn('/p/scripts/xmux-agent spawn', SPAWN_JSON)).toBe(false)
  expect(isSpawn('/p/scripts/mux-agent.sh spawn', SPAWN_JSON)).toBe(false)
  expect(isSpawn('/p/scripts/mux-agent spawn', TAGGED_JSON)).toBe(false)
})

test('isSpawn rejects a bare mention, another verb or file, and every refused, mixed or missing reply', () => {
  expect(isSpawn('grep -n "mux-agent.ps1 spawn" README.md', 'README.md:27:pwsh -File "<entry>" spawn')).toBe(false)
  expect(isSpawn(`${E} spawner`, SPAWN_JSON)).toBe(false)
  expect(isSpawn(`${E} list`, LISTED)).toBe(false)
  expect(isSpawn('pwsh -File C:/p/xmux-agent.ps1 spawn', SPAWN_JSON)).toBe(false)
  expect(isSpawn('pwsh -File C:/p/mux-agent.ps1.bak spawn', SPAWN_JSON)).toBe(false)
  expect(isSpawn(`${E} spawn`, TAGGED_JSON)).toBe(false)
  expect(isSpawn(`${E} spawn -Text`, TAGGED_TEXT)).toBe(false)
  expect(isSpawn(`${E} spawn`, '')).toBe(false)
  expect(isSpawn(`${E} spawn`, undefined)).toBe(false)
})

test('ok and verb must come from one reply: a refused spawn then a successful list does not count', () => {
  expect(isSpawn(`${E} spawn`, `${TAGGED_JSON}${LISTED}`)).toBe(false)
  expect(isSpawn(`${E} spawn`, `${TAGGED_JSON}\n${LISTED}`)).toBe(false)
})

test('the accepted false positive: a command that mentions spawn and prints a spawn reply', () => {
  expect(isSpawn(`echo 'mux-agent.ps1 spawn' '${SPAWN_JSON}'`, SPAWN_JSON)).toBe(true)
})

test('clean drops C0, DEL and C1 and turns tabs into spaces', () => {
  expect(clean('a\u0000b\u0007c\u001b[1md\te\u007ff\u0080g\u009bh\u009fi\u00a0j')).toBe('abc[1md efghi\u00a0j')
})

test('parseList maps every failure to undefined and cleans every string', () => {
  const good = '{"ok":true,"verb":"list","running":0,"agents":[]}'
  expect(parseList(0, good)).toEqual({ running: 0, agents: [] })
  expect(parseList(1, good)).toBe(undefined)
  expect(parseList(0, 'not json')).toBe(undefined)
  expect(parseList(0, '{"ok":false,"verb":"list","code":1,"check":"endpoint"}')).toBe(undefined)
  expect(parseList(0, '{"ok":false,"verb":"list","running":1,"agents":[]}')).toBe(undefined)
  expect(parseList(0, '{"ok":true,"verb":"list","agents":[]}')).toBe(undefined)
  expect(parseList(0, '{"ok":true,"verb":"list","running":0}')).toBe(undefined)
  const dirty = JSON.stringify({ ok: true, running: 1, agents: [{ name: 'a\u001b', client: 'c\u0007', state: 's\u009b', doing: 'd\te', age: 1 }] })
  expect(parseList(0, dirty)).toEqual({ running: 1, agents: [{ name: 'a', client: 'c', state: 's', doing: 'd e', age: 1 }] })
})

test('formatAge uses s, m and h, floored', () => {
  expect([0, 59, 60, 3599, 3600, 7199].map(formatAge)).toEqual(['0s', '59s', '1m', '59m', '1h', '1h'])
})
