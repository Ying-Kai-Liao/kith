import { useState } from 'react'
import { Icon } from '../Icon'
import { ModalHeader, Sheet, SheetPage } from '../Overlay'
import './kith.css'

export interface ToolDetail {
  command?: string
  output?: string
}

/** A tool call in the transcript: 44 tall, radius 10, panel fill. Verb and object left, the result right in mono. */
export function ToolRow({ verb, object, result, state = 'done', detail, onPeek }: {
  /** "Edited", "Ran tests". */
  verb: string
  /** The file, command or target. */
  object?: string
  /** "+12 -3", "passed", "running". */
  result?: string
  state?: 'running' | 'done' | 'error'
  /** With a command or output, the row opens a sheet showing them. */
  detail?: ToolDetail
  /** A handoff's row: it opens the peek sheet in place of the command and output. */
  onPeek?: () => void
}) {
  const [open, setOpen] = useState(false)
  const openable = !!(detail?.command || detail?.output || onPeek)
  const label = `${verb}${object ? ` ${object}` : ''}${result ? `, ${result}` : ''}`
  const body = (
    <>
      <span className="kc-tool-text kith-text-body">
        <span className="kc-tool-verb">{verb}</span>
        {object ? <span className="kc-tool-object"> {object}</span> : null}
      </span>
      {result ? <span className={`kc-tool-result kith-text-code ${state}`}>{result}</span> : null}
      {openable ? <Icon name="chevron.right" size={9} weight="semibold" className="kc-tool-chevron" /> : null}
    </>
  )
  if (!openable) return <div className="kc-tool" aria-label={label}>{body}</div>
  return (
    <>
      <button className="kc-tool openable" onClick={onPeek ?? (() => setOpen(true))} aria-label={`${label}. ${onPeek ? 'Open the handoff' : 'Show details'}`}>
        {body}
      </button>
      <Sheet open={open} onClose={() => setOpen(false)} width={640} dismissOnDim>
        <SheetPage>
          <ModalHeader title={`${verb}${object ? ` ${object}` : ''}`} onClose={() => setOpen(false)} />
          <div className="kc-tool-sheet">
            {detail?.command ? (
              <>
                <div className="kith-text-label kc-tool-sheet-label">COMMAND</div>
                <pre className="kc-code kith-text-code selectable">{detail.command}</pre>
              </>
            ) : null}
            {detail?.output ? (
              <>
                <div className="kith-text-label kc-tool-sheet-label">OUTPUT</div>
                <pre className={`kc-code kith-text-code selectable ${state === 'error' ? 'error' : ''}`}>{detail.output}</pre>
              </>
            ) : null}
          </div>
        </SheetPage>
      </Sheet>
    </>
  )
}
