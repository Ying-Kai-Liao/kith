import assert from 'node:assert/strict'
import { test } from 'node:test'
import { isFetchableUrl, isPrivateHost, NO_PREVIEW, parseHead, PreviewCache } from './link-preview.ts'

const base = 'https://news.example.com/a/b.html'

test('head: og first, then twitter, relative urls resolve, entities decode, body is ignored', () => {
  const h = parseHead(`<html><head>
    <meta name="twitter:image" content="/tw.png">
    <meta property="og:image" content='img/og.jpg?a=1&amp;b=2'>
    <link rel="shortcut icon" href="//cdn.example.com/f.ico">
    <link rel="apple-touch-icon" href="/touch.png">
    </head><body><meta property="og:image" content="/late.png"></body>`, base)
  assert.deepEqual(h.images, ['https://news.example.com/a/img/og.jpg?a=1&b=2', 'https://news.example.com/tw.png'])
  assert.deepEqual(h.icons, ['https://cdn.example.com/f.ico'])
  assert.deepEqual(h.touchIcons, ['https://news.example.com/touch.png'])
})

test('head: missing tags and non-http values give nothing', () => {
  const h = parseHead('<head><meta property="og:image" content="javascript:alert(1)"><title>x</title></head>', base)
  assert.deepEqual(h, { images: [], touchIcons: [], icons: [] })
})

test('private hosts are refused', () => {
  for (const h of ['localhost', 'a.local', '127.0.0.1', '10.1.2.3', '192.168.0.9', '172.16.0.1', '172.31.9.9', '169.254.1.1', '::1', '[::1]', 'fd00::1', 'fe80::1', '::ffff:127.0.0.1']) {
    assert.ok(isPrivateHost(h), h)
  }
  for (const h of ['example.com', '8.8.8.8', '172.32.0.1', '172.15.0.1']) assert.ok(!isPrivateHost(h), h)
  assert.ok(!isFetchableUrl('ftp://example.com/'))
  assert.ok(!isFetchableUrl('http://localhost:3000/'))
  assert.ok(!isFetchableUrl('https://user:pw@example.com/'))
  assert.ok(isFetchableUrl('https://example.com/x'))
})

test('cache: hit, negative hit, expiry, cap', () => {
  let t = 1000
  const c = new PreviewCache(() => t, 2, 100)
  assert.equal(c.get('a'), undefined)
  c.set('a', { image: 'data:x', icon: null })
  c.set('n', NO_PREVIEW)
  assert.equal(c.get('a')?.image, 'data:x')
  assert.deepEqual(c.get('n'), NO_PREVIEW)
  t += 101
  assert.equal(c.get('a'), undefined)
  assert.equal(c.get('n'), undefined)
  c.set('1', NO_PREVIEW)
  c.set('2', NO_PREVIEW)
  c.set('3', NO_PREVIEW)
  assert.equal(c.get('1'), undefined)
  assert.ok(c.get('3'))
})
