import assert from 'node:assert/strict'
import { test } from 'node:test'
import { goesTo, type GoesToAgent } from './goes-to.ts'

const a = (name: string, lastAt: number, selected = false): GoesToAgent => ({ key: name, name, lastAt, selected })
const crew = [a('coder', 5), a('writer', 9), a('Ada Lovelace', 1), a('Ada', 3)]

test('most recent agent when nothing is selected, with the next two as alternatives', () => {
  const r = goesTo(crew, 'fix the bug')
  assert.equal(r.target?.name, 'writer')
  assert.deepEqual(r.alternatives.map((x) => x.name), ['coder', 'Ada'])
  assert.equal(r.all.length, 4)
})

test('the selected chat beats recency', () => {
  const r = goesTo([a('coder', 5, true), a('writer', 9)], 'hi')
  assert.equal(r.target?.name, 'coder')
  assert.deepEqual(r.alternatives.map((x) => x.name), ['writer'])
})

test('first agent when none has history', () => {
  assert.equal(goesTo([a('x', 0), a('y', 0)], 'hi').target?.name, 'x')
})

test('an @mention wins and is stripped from the text', () => {
  const r = goesTo(crew, '@coder  please look')
  assert.equal(r.how, 'mention')
  assert.equal(r.target?.name, 'coder')
  assert.equal(r.text, 'please look')
  assert.deepEqual(r.alternatives, [])
})

test('a name with spaces and a shorter prefix name', () => {
  assert.equal(goesTo(crew, '@ada lovelace hi').target?.name, 'Ada Lovelace')
  assert.equal(goesTo(crew, '@ada hi').target?.name, 'Ada')
})

test('an unknown or partial @name has no target', () => {
  assert.equal(goesTo(crew, '@nobody hi').how, 'unknown')
  assert.equal(goesTo(crew, '@nobody hi').target, null)
  assert.equal(goesTo(crew, '@code').target, null)
})

test('an override from the pill is used; no agents gives none', () => {
  assert.equal(goesTo(crew, 'hi', 'Ada').target?.name, 'Ada')
  assert.equal(goesTo([], 'hi').how, 'none')
  assert.deepEqual(goesTo([a('solo', 1)], 'hi').alternatives, [])
})
