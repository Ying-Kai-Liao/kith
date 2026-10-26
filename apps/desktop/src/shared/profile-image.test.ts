import assert from 'node:assert/strict'
import { test } from 'node:test'
import { linkedPhotos, profileImageProblem } from './profile-image.ts'

test('only png, jpeg and webp up to 10 MB pass', () => {
  assert.equal(profileImageProblem({ type: 'image/png', size: 1000 }), null)
  assert.equal(profileImageProblem({ type: 'image/webp', size: 10 * 1024 * 1024 }), null)
  assert.match(profileImageProblem({ type: 'image/gif', size: 1000 })!, /PNG, JPG or WebP/)
  assert.match(profileImageProblem({ type: 'image/jpeg', size: 10 * 1024 * 1024 + 1 })!, /10 MB/)
  assert.ok(profileImageProblem({ type: 'image/png', size: 0 }))
})

test('linked photos skip accounts without an https image and strip the oauth_ prefix', () => {
  const photos = linkedPhotos([
    { provider: 'oauth_google', image_url: 'https://img.example/g.png' },
    { provider: 'oauth_github', image_url: '' },
    { provider: 'github', image_url: 'http://insecure/x.png' },
    { provider: 'github', image_url: 'https://img.example/h.png' },
  ])
  assert.deepEqual(photos, [
    { provider: 'google', imageURL: 'https://img.example/g.png' },
    { provider: 'github', imageURL: 'https://img.example/h.png' },
  ])
  assert.deepEqual(linkedPhotos(undefined), [])
})
