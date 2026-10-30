// A door's preload (kith_door.py). Nothing in a door's sandbox may bind a socket (seatbelt can't hold a listen to
// loopback), so every Node server takes the next listening socket the kernel opened on 127.0.0.1 and passed in, whatever
// it asked for, and Chrome is driven over a pipe instead of a debugging port.
import net from 'node:net';

const fds = (process.env.KITH_DOOR_LISTEN_FDS || '').split(',').filter(Boolean).map(Number);
const listen = net.Server.prototype.listen;
let next = 0;

net.Server.prototype.listen = function (...args) {
  const done = typeof args[args.length - 1] === 'function' ? [args[args.length - 1]] : [];
  const fd = fds[next++];
  if (fd === undefined) throw new Error('the door has no socket left for a server');
  return listen.call(this, { fd }, ...done);
};

if (process.env.KITH_DOOR_CHROME) {
  // resolved from the door's cache, the same copy the door's own code imports
  const { default: puppeteer } = await import('puppeteer-core');
  const launch = puppeteer.launch;
  puppeteer.launch = function (options = {}) {
    // puppeteer only uses the pipe when no --remote-debugging-* argument is given
    const args = (options.args || []).filter((a) => !String(a).startsWith('--remote-debugging-'));
    return launch.call(this, { ...options, args, pipe: true });
  };
}
