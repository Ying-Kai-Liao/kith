/** The folder an approval is for: the last part of the card's cwd, or null when there is none. */
export const folderOf = (cwd?: string | null): string | null => cwd?.split('/').filter(Boolean).pop() ?? null

/** The secondary approval button: "Always allow in <folder>", or just "Always allow" with no folder. */
export const alwaysAllowLabel = (cwd?: string | null): string => {
  const folder = folderOf(cwd)
  return folder ? `Always allow in ${folder}` : 'Always allow'
}

/** The notch island's short words for an approval option, by kind; anything else keeps the host's name. */
export function islandOptionLabel(kind: string, name: string): string {
  switch (kind) {
    case 'allow_once':
      return 'Allow'
    case 'reject_once':
      return 'Deny'
    case 'allow_always':
      return 'Always'
    default:
      return name
  }
}
