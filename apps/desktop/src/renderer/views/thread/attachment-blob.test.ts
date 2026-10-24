import assert from 'node:assert/strict'
import { test } from 'node:test'
import { attachmentBlob } from './attachment-blob.ts'

test('an attachment preview is an opaque download type, even when the bytes are HTML or SVG', async () => {
  const html = new TextEncoder().encode('<script>alert(1)</script>')
  const blob = attachmentBlob(html)
  assert.equal(blob.type, 'application/octet-stream')
  assert.equal(await blob.text(), '<script>alert(1)</script>')
  assert.equal(attachmentBlob(new TextEncoder().encode('<svg onload=alert(1)>').buffer).type, 'application/octet-stream')
})
