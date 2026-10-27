import { useEffect, useRef, useState } from 'react'
import { providerPhotoLabel, profileImageProblem, type ProfileImageSource } from '@shared/profile-image'
import { Button, Spinner } from '../../components/Controls'
import { SKY_PRESETS, skyCropRect, type SkyLook } from '../../components/kith/logic'
import { ModalHeader, SheetPage } from '../../components/Overlay'
import { Text } from '../../components/Text'
import { useModel } from '../../lib/observable'
import { account } from '../../store/account'
import skyImage from '../../assets/sky/sky-header-day.webp'

const SIZE = 512

/** A preset on a square canvas: the palette gradient, then the cloud crop over it. Tiles and the upload both come from this. */
async function drawSky(look: SkyLook, size: number): Promise<HTMLCanvasElement> {
  const img = new Image()
  img.src = skyImage
  await img.decode()
  const canvas = document.createElement('canvas')
  canvas.width = canvas.height = size
  const ctx = canvas.getContext('2d')!
  const base = ctx.createLinearGradient(0, 0, 0, size)
  base.addColorStop(0, look.top)
  base.addColorStop(1, look.bottom)
  ctx.fillStyle = base
  ctx.fillRect(0, 0, size, size)
  const r = skyCropRect(img.naturalWidth, img.naturalHeight, look, size, look.zoom)
  ctx.globalAlpha = look.cloud / 100
  ctx.drawImage(img, r.x, r.y, r.w, r.h)
  return canvas
}

async function renderSky(look: SkyLook): Promise<ProfileImageSource> {
  const canvas = await drawSky(look, SIZE)
  const blob = await new Promise<Blob | null>((resolve) => canvas.toBlob(resolve, 'image/png'))
  if (!blob) throw new Error('render')
  return { kind: 'bytes', bytes: await blob.arrayBuffer(), type: 'image/png' }
}

/** The tile for a preset: the same canvas drawing the upload uses, shown at tile size. */
function SkyTile({ look }: { look: SkyLook }) {
  const [src, setSrc] = useState<string | null>(null)
  useEffect(() => {
    let live = true
    void drawSky(look, 168).then((c) => live && setSrc(c.toDataURL()))
    return () => {
      live = false
    }
  }, [look])
  return <span className="photo-tile-sky" style={src ? { backgroundImage: `url(${src})` } : undefined} aria-hidden />
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
          {SKY_PRESETS.map((look) =>
            tile(`sky-${look.name}`, `Sky ${look.name}`, () => renderSky(look), <SkyTile look={look} />),
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
