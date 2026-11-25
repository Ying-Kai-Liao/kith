import assert from 'node:assert/strict'
import { test } from 'node:test'
import { mockInvitePreviewProvider, type InvitePreviewProvider } from '../../shared/invite-preview.ts'
import { loadPreview, pickDoneTarget } from './invite-preview-logic.ts'

const link = { computerId: 'cmp_1', inviteId: 'inv_1', token: 'T'.repeat(24) }

test('the mock preview is deterministic per invite', async () => {
  assert.deepEqual(await mockInvitePreviewProvider.preview(link), await mockInvitePreviewProvider.preview({ ...link }))
  const p = await mockInvitePreviewProvider.preview(link)
  assert.equal(p.inviterName, 'Kai')
  assert.equal(p.computerName, "Kai's Mac mini")
  assert.deepEqual(p.items.map((i) => [i.kind, i.name]), [['agent', 'Finance'], ['room', 'Family']])
})

test('a failing provider gives null, so the page shows the generic invite', async () => {
  const failing: InvitePreviewProvider = { preview: async () => Promise.reject(new Error('down')) }
  assert.equal(await loadPreview(failing, link), null)
  assert.ok(await loadPreview(mockInvitePreviewProvider, link))
})

test('the done target is the first item matching a bot by id or name, else null', async () => {
  const { items } = await mockInvitePreviewProvider.preview(link)
  assert.equal(pickDoneTarget(items, [{ id: 'b1', name: 'Other' }]), null)
  assert.equal(pickDoneTarget(items, [{ id: 'finance', name: 'x' }]), 'finance')
  assert.equal(pickDoneTarget(items, [{ id: 'b2', name: 'family ' }, { id: 'b3', name: 'FINANCE' }]), 'b3')
  assert.equal(pickDoneTarget([], [{ id: 'b1', name: 'Finance' }]), null)
})
