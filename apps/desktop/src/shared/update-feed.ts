/**
 * Where the app finds its own releases: Kith's own Releases page (the `publish:` block in electron-builder.yml names the same repo). Kith's builds must never look at (or
 * install from) another project's feed, and Kith Dev sets no feed at all (electron-builder.dev.yml).
 */
export const UPDATE_FEED: string | null = 'https://github.com/Ying-Kai-Liao/kith/releases'
