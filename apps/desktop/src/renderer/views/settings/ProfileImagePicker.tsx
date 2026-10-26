import { useRef, useState } from 'react'
import { providerPhotoLabel, profileImageProblem, type ProfileImageSource } from '@shared/profile-image'
import { Button, Spinner } from '../../components/Controls'
import { SKY_PRESETS, SKY_TONE_STOPS, skyCropRect, type SkyPick } from '../../components/kith/logic'
import { ModalHeader, SheetPage } from '../../components/Overlay'
import { Text } from '../../components/Text'
import { useModel } from '../../lib/observable'
import { account } from '../../store/account'
import skyImage from '../../assets/sky/sky-header-day.webp'

const SIZE = 512

/** A preset as a square PNG: the same crop and tone wash as the `.kc-sky` patch, drawn on a canvas. */
async function renderSky(pick: SkyPick): Promise<ProfileImageSource> {
  const img = new Image()
  img.src = skyImage
  await img.decode()
  const canvas = document.createElement('canvas')
  canvas.width = canvas.height = SIZE
  const ctx = canvas.getContext('2d')!
  const r = skyCropRect(img.naturalWidth, img.naturalHeight, pick, SIZE)
  ctx.drawImage(img, r.x, r.y, r.w, r.h)
  const wash = ctx.createLinearGradient(0, 0, 0, SIZE)
  for (const [at, color] of SKY_TONE_STOPS[pick.tone]) wash.addColorStop(at, color)
  ctx.fillStyle = wash
  ctx.fillRect(0, 0, SIZE, SIZE)
  const blob = await new Promise<Blob | null>((resolve) => canvas.toBlob(resolve, 'image/png'))
  if (!blob) throw new Error('render')
  return { kind: 'bytes', bytes: await blob.arrayBuffer(), type: 'image/png' }
}

/** Settings → Account → Change photo: a linked account's own photo, one of the Kith skies, or a file. */
export function ProfileImagePicker({ onClose }: { onClose: () => void }) {
  useModel(account)
  const [working, setWorking] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const file = useRef<HTMLInputElement>(null)
  const photos = account.user?.photos ?? []

  const apply = async (key: string, source: () => Promise<ProfileImageSource>) => {
    if (working) return
    setWorking(key)
    setError(null)
    try {
      const failure = await account.setProfileImage(await source())
      if (failure) setError(failure)
      else onClose()
    } catch {
      setError("Couldn't change your photo. Check your connection and try again.")
    } finally {
      setWorking(null)
    }
  }

  const upload = (f: File | undefined) => {
    if (!f) return
    const problem = profileImageProblem(f)
    if (problem) return setError(problem)
    void apply('upload', async () => ({ kind: 'bytes', bytes: await f.arrayBuffer(), type: f.type }))
  }

  const tile = (key: string, label: string, source: () => Promise<ProfileImageSource>, content: React.ReactNode) => (
    <button key={key} className="photo-tile" aria-label={label} title={label} disabled={working !== null} onClick={() => void apply(key, source)}>
      {content}
      {working === key ? (
        <span className="photo-tile-busy">
          <Spinner size={18} />
        </span>
      ) : null}
    </button>
  )

  return (
    <SheetPage className="photo-picker">
      <ModalHeader title="Change photo" />
      {photos.map((p) => (
        <section key={p.provider} className="photo-section">
          <Text size="caption" tone="secondary" weight="semibold">From {providerPhotoLabel(p.provider)}</Text>
          <div className="photo-grid">
            {tile(`from-${p.provider}`, `Use your ${providerPhotoLabel(p.provider)} photo`, async () => ({ kind: 'url', url: p.imageURL }), <img className="photo-tile-image" src={p.imageURL} alt="" />)}
          </div>
        </section>
      ))}
      <section className="photo-section">
        <Text size="caption" tone="secondary" weight="semibold">Kith defaults</Text>
        <div className="photo-grid">
          {SKY_PRESETS.map((pick, i) =>
            tile(`sky-${i}`, `Sky ${i + 1}`, () => renderSky(pick), <span className={`kc-sky kc-sky-${pick.tone}`} style={{ width: 56, height: 56, '--sky-x': `${pick.x}%`, '--sky-y': `${pick.y}%` } as React.CSSProperties} aria-hidden />),
          )}
        </div>
      </section>
      <section className="photo-section">
        <Text size="caption" tone="secondary" weight="semibold">Upload</Text>
        <div>
          <input ref={file} type="file" accept="image/png,image/jpeg,image/webp" hidden onChange={(e) => { upload(e.target.files?.[0]); e.target.value = '' }} />
          <Button kind="secondary" disabled={working !== null} onClick={() => file.current?.click()}>
            {working === 'upload' ? 'Uploading…' : 'Upload…'}
          </Button>
        </div>
      </section>
      {error ? <Text as="div" size="caption" tone="attention" role="alert">{error}</Text> : null}
    </SheetPage>
  )
}
