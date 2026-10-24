// Two things Clerk's bundled SDK tries that the page's own rules forbid (`script-src 'self'`, Trusted Types). Both are
// refused here, quietly and on purpose, rather than left to show up as console errors. A module of its own, imported
// before Clerk's: imports run in order, so this has happened before Clerk's code is evaluated.

// 1. A timer worker from a blob: URL. Trusted Types blocks that (and allowing it would need `worker-src blob:`), so there
//    is no Worker for it to find, and it uses main-thread timers, which it does anyway when workers are unavailable.
//    Nothing else in the page uses a Worker.
Object.defineProperty(globalThis, 'Worker', { value: undefined, configurable: true, writable: true })

// 2. Clerk Protect (an instance setting for fraud checks) asks the page to load a script from cdn.protect.clerk.com. No
//    third-party script runs here (design §4.3 item 4): a script element only ever gets a source on this page's own origin.
const sameOrigin = (value: unknown) => {
  try {
    return new URL(String(value), location.href).origin === location.origin
  } catch {
    return false
  }
}

const setAttribute = Element.prototype.setAttribute
Element.prototype.setAttribute = function (this: Element, name: string, value: string) {
  if (this instanceof HTMLScriptElement && name.toLowerCase() === 'src' && !sameOrigin(value)) return
  return setAttribute.call(this, name, value)
}

const src = Object.getOwnPropertyDescriptor(HTMLScriptElement.prototype, 'src')
if (src?.set) {
  Object.defineProperty(HTMLScriptElement.prototype, 'src', {
    ...src,
    set(this: HTMLScriptElement, value: string) {
      if (sameOrigin(value)) src.set!.call(this, value)
    },
  })
}

export {}
