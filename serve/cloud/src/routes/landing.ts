// The home page at https://kithbot.app/. landing.html is designer's page as a whole document; its images and
// fonts are static assets under site/ (img/, fonts/), served by the Workers assets router with their own cache
// headers (site/_headers). The page works without JavaScript; its one script only plays the room and the Copy button.

import type { Ctx } from "../api";
import HTML from "./landing.html";

/** The page's one inline script, as the browser hashes it. */
export const LANDING_SCRIPT = /<script>([\s\S]*?)<\/script>/.exec(HTML)![1]!;

let scriptHash: Promise<string> | undefined;
const hashOfScript = () =>
  (scriptHash ??= crypto.subtle.digest("SHA-256", new TextEncoder().encode(LANDING_SCRIPT)).then(
    (d) => "sha256-" + btoa(String.fromCharCode(...new Uint8Array(d))),
  ));

export async function landingPage(_c: Ctx): Promise<Response> {
  const hash = await hashOfScript();
  return new Response(HTML, {
    headers: {
      "Content-Type": "text/html; charset=utf-8",
      // Static page: a short public cache is enough.
      "Cache-Control": "public, max-age=300",
      "X-Content-Type-Options": "nosniff",
      "Content-Security-Policy": `default-src 'none'; script-src '${hash}'; style-src 'unsafe-inline'; img-src 'self'; font-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'`,
    },
  });
}
