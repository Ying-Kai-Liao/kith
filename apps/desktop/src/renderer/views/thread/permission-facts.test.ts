import assert from 'node:assert/strict'
import { test } from 'node:test'
import { cardFacts, factNote, factPieces, factSymbol, placeLine } from './permission-facts.ts'

test('a fact is cut into plain and bold pieces in order', () => {
  assert.deepEqual(factPieces({ text: 'Runs a Python script written into the command, and ffmpeg', strong: ['Python script', 'ffmpeg'] }), [
    { text: 'Runs a ', bold: false },
    { text: 'Python script', bold: true },
    { text: ' written into the command, and ', bold: false },
    { text: 'ffmpeg', bold: true },
  ])
  // A repeated word bolds the one the host meant: the second, after the first was passed.
  assert.deepEqual(factPieces({ text: 'Saves into dl dl', strong: ['dl', 'dl'] }).filter((p) => p.bold).length, 2)
  assert.deepEqual(factPieces({ text: 'A shell command: curl', strong: ['A shell command'] }), [
    { text: 'A shell command', bold: true },
    { text: ': curl', bold: false },
  ])
})

test('a fact with no bold parts, or one the text does not contain, is plain', () => {
  assert.deepEqual(factPieces({ text: 'Kith can’t read it' }), [{ text: 'Kith can’t read it', bold: false }])
  assert.deepEqual(factPieces({ text: 'Opens docs.rs', strong: ['elsewhere'] }), [{ text: 'Opens docs.rs', bold: false }])
})

test("the muted words are the host's label, then this Mac for the line that says where it runs", () => {
  assert.equal(factNote({ muted: 'its folder', place: true }, 'Mac mini'), 'its folder · Mac mini')
  assert.equal(factNote({ place: true }, 'Mac mini'), 'Mac mini')
  assert.equal(factNote({ muted: 'its scratch' }, 'Mac mini'), 'its scratch')
  assert.equal(factNote({}, 'Mac mini'), '')
})

test('every icon word has a symbol, and an unknown one draws as a document', () => {
  for (const icon of ['terminal', 'down', 'globe', 'pencil', 'trash', 'stop', 'folder', 'eye', 'doc']) assert.ok(factSymbol(icon))
  assert.equal(factSymbol('down'), 'arrow.down.circle')
  assert.equal(factSymbol('stop'), 'stop.circle')
  assert.equal(factSymbol('never-heard-of-it'), 'doc')
})

test('no facts, or only broken ones, means today’s card', () => {
  assert.equal(cardFacts({}), null)
  assert.equal(cardFacts({ facts: [] }), null)
  assert.equal(cardFacts({ facts: [{ icon: 'terminal', text: '' }] }), null)
  assert.deepEqual(cardFacts({ facts: [{ icon: 'terminal', text: '' }, { icon: 'globe', text: 'Opens docs.rs' }] }), [{ icon: 'globe', text: 'Opens docs.rs' }])
})

test('the Mac stays named: Runs on <Mac> unless a fact already says where it runs', () => {
  assert.equal(placeLine([{}], 'Runs on Mac mini · from coder', 'from coder'), 'Runs on Mac mini · from coder')
  assert.equal(placeLine([{}, { place: true }], 'Runs on Mac mini · from coder', 'from coder'), 'from coder')
  assert.equal(placeLine([{ place: true }], 'Runs on Mac mini'), undefined)
})
