import assert from 'node:assert/strict'
import { test } from 'node:test'
import { firstLink, parseShareLink, shareMessage } from './share-link.ts'

const schemes = ['kith', 'dev.kith.app']
const enc = encodeURIComponent

test('a share link gives its link, title and text', () => {
  const item = parseShareLink(`kith://share?url=${enc('https://example.com/a?b=1')}&title=${enc('A post')}`, schemes)
  assert.deepEqual(item, { link: 'https://example.com/a?b=1', title: 'A post', text: null })
  assert.ok(parseShareLink(`KITH://SHARE?url=${enc('https://example.com')}`, schemes))
  assert.ok(parseShareLink(`dev.kith.app://share?url=${enc('https://example.com')}`, schemes))
  assert.ok(parseShareLink(`kith:share?url=${enc('https://example.com')}`, schemes))
})

test('a link inside shared text is found when there is no url', () => {
  const item = parseShareLink(`kith://share?text=${enc("Look at this https://example.com/a, it's good")}`, schemes)
  assert.equal(item?.link, 'https://example.com/a')
  assert.equal(firstLink('no address here'), null)
  assert.equal(firstLink('see (https://example.com/x) now'), 'https://example.com/x')
})

test('text with no link is still something to send', () => {
  assert.deepEqual(parseShareLink(`kith://share?text=${enc('remember the milk')}`, schemes), { link: null, title: null, text: 'remember the milk' })
})

test('only web addresses count as a link', () => {
  for (const url of ['file:///etc/passwd', 'javascript:alert(1)', 'kith://invite?c=a&i=b&t=12345678', 'not a url']) {
    assert.equal(parseShareLink(`kith://share?url=${enc(url)}`, schemes), null, url)
  }
  assert.equal(parseShareLink(`kith://share?url=${enc('file:///etc/passwd')}&text=${enc('hello')}`, schemes)?.link, null)
})

test('other links and other schemes are not shares', () => {
  assert.equal(parseShareLink('kith://invite?c=a&i=b&t=12345678', schemes), null)
  assert.equal(parseShareLink(`https://share?url=${enc('https://example.com')}`, schemes), null)
  assert.equal(parseShareLink('kith://share', schemes), null)
  assert.equal(parseShareLink('kith://share?url=&title=x', schemes), null)
})

test('long parts are cut', () => {
  const item = parseShareLink(`kith://share?text=${enc('x'.repeat(9000))}&title=${enc('t'.repeat(900))}`, schemes)
  assert.equal(item?.text?.length, 4000)
  assert.equal(item?.title?.length, 300)
})

test('the message is the note, then the title, then the link', () => {
  const link = 'https://example.com/post'
  assert.equal(shareMessage({ link, title: 'A post', text: null }, 'summarise this'), `summarise this\n\nA post\n\n${link}`)
  assert.equal(shareMessage({ link, title: null, text: null }, ''), link)
  // A title or caption that is the address, or holds it, says nothing more.
  assert.equal(shareMessage({ link, title: link, text: link }, '  '), link)
  assert.equal(shareMessage({ link, title: null, text: `Read ${link} now` }, ''), link)
  assert.equal(shareMessage({ link: null, title: null, text: ' remember the milk ' }, 'todo'), 'todo\n\nremember the milk')
})
