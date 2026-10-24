import { resolve } from 'node:path'
import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'
import { version } from './package.json'

// The member-only web client (design 2026-10-25-web-client), built to dist-web/ for an assets-only Worker on its own
// origin. Not the website demo (vite.demo.config.ts) and it never writes into the repo-root web/ folder.
// The cloud and the Clerk instance are fixed here at build time, never taken from an invite link or from the cloud.
// Dev defaults: the dev cloud and Clerk dev instance adjusted-starfish-4735 (the publishable key is public).
const DEV_CLOUD = 'https://kith-cloud-dev.lykevin890919.workers.dev'
const DEV_CLERK_KEY = 'pk_test_YWRqdXN0ZWQtc3RhcmZpc2gtNDczNS5jbGVyay5hY2NvdW50cy5kZXYk'

const fromEnv = (name: string, fallback: string) => process.env[name]?.trim() || fallback

export default defineConfig({
  root: resolve('src/renderer/web'),
  publicDir: false,
  // Absolute: the page is served at /join, /, and wherever the SPA fallback lands.
  base: '/',
  resolve: { alias: { '@shared': resolve('src/shared') } },
  plugins: [react()],
  define: {
    __KITH_WEB__: JSON.stringify({
      cloudURL: fromEnv('KITH_WEB_CLOUD_URL', DEV_CLOUD),
      clerkKey: fromEnv('KITH_WEB_CLERK_PUBLISHABLE_KEY', DEV_CLERK_KEY),
      version,
    }),
  },
  build: {
    outDir: resolve('dist-web'),
    emptyOutDir: true,
    chunkSizeWarningLimit: 3000,
    // Safari 17+ / Chrome 120+ / Firefox 120+ all have the Ed25519 WebCrypto this client needs anyway.
    target: 'es2022',
    // No inline scripts or styles injected by the bundler: script-src 'self'.
    modulePreload: { polyfill: false },
  },
})
