/** Picking a profile image: what may be uploaded and which linked accounts offer their own photo. Pure, so node --test covers it. */

export const PROFILE_IMAGE_MAX_BYTES = 10 * 1024 * 1024
export const PROFILE_IMAGE_TYPES = ['image/png', 'image/jpeg', 'image/webp'] as const

/** Null when the file may be uploaded, else the sentence to show under the picker. */
export function profileImageProblem(file: { type: string; size: number }): string | null {
  if (!(PROFILE_IMAGE_TYPES as readonly string[]).includes(file.type)) return 'Choose a PNG, JPG or WebP image.'
  if (file.size <= 0) return 'That image is empty.'
  if (file.size > PROFILE_IMAGE_MAX_BYTES) return 'That image is over 10 MB.'
  return null
}

export interface LinkedPhoto {
  /** `google` or `github`. */
  provider: string
  imageURL: string
}

const PROVIDER_LABEL: Record<string, string> = { google: 'Google', github: 'GitHub' }
export const providerPhotoLabel = (provider: string) => PROVIDER_LABEL[provider] ?? provider

/** One photo per linked external account that has an https image, in Clerk's order. Clerk names providers `oauth_google` (API) or `google`. */
export function linkedPhotos(accounts: readonly { provider?: string | null; image_url?: string | null }[] | undefined): LinkedPhoto[] {
  const out: LinkedPhoto[] = []
  for (const a of accounts ?? []) {
    const imageURL = a.image_url?.trim()
    const provider = a.provider?.replace(/^oauth_/, '')
    if (!provider || !imageURL || !imageURL.startsWith('https://')) continue
    if (out.some((p) => p.provider === provider)) continue
    out.push({ provider, imageURL })
  }
  return out
}

/** What the picker hands the account: bytes to upload, or an https URL to download first. */
export type ProfileImageSource = { kind: 'bytes'; bytes: ArrayBuffer; type: string } | { kind: 'url'; url: string }
