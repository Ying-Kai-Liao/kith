import { useCallback, useEffect, useRef, useState } from 'react'
import { callPresence } from '../../store/call-presence'
import { VoiceSettings } from '../call/voice'
import { DictationTranscript, joinDictated } from './dictation-transcript.ts'

/** The running dictation's stop: one recognizer at a time, and a call stops dictation first. */
let stopActive: (() => void) | null = null

export function stopDictation() {
  stopActive?.()
}

/**
 * Dictation into the composer with on-device speech (the speech helper; macOS). Everything heard
 * shows in the box after what was already typed, and it keeps listening through pauses (a
 * recognition that ends is started again quietly) until `stop`.
 */
export function useDictation(draft: string, setDraft: (text: string) => void) {
  const [listening, setListening] = useState(false)
  /** The box when dictation started. */
  const before = useRef('')
  const transcript = useRef(new DictationTranscript())
  const off = useRef<(() => void) | null>(null)
  /** This recognition said `started`, and when it was asked for: a helper that fails at once isn't started again. */
  const started = useRef(false)
  const lastStart = useRef(0)
  const write = useRef(setDraft)
  write.current = setDraft

  const stop = useCallback(() => {
    off.current?.()
    off.current = null
    window.kith.speech.stop()
    setListening(false)
    if (stopActive === stop) stopActive = null
  }, [])

  const recognize = () => {
    started.current = false
    lastStart.current = Date.now()
    // The helper's start ends the recognition before it.
    window.kith.speech.start(VoiceSettings.language() || null)
  }

  const start = () => {
    // A call has the microphone.
    if (callPresence.active > 0) return
    stopActive?.()
    before.current = draft
    transcript.current = new DictationTranscript()
    off.current = window.kith.speech.onEvent((e) => {
      if (e.type === 'started') started.current = true
      if (e.type === 'partial' || e.type === 'final') {
        transcript.current.heard(e.text, e.type === 'final' || !!e.ends)
        write.current(joinDictated(before.current, transcript.current.text))
        // The recognition is over after a final (a long pause, its time limit): listen on in a new one.
        if (e.type === 'final') recognize()
      }
      if (e.type === 'stopped' || e.type === 'error') {
        if (!started.current || Date.now() - lastStart.current < 1000) return stop()
        transcript.current.restart()
        recognize()
      }
    })
    recognize()
    stopActive = stop
    setListening(true)
  }

  // A composer that goes away (another chat) stops listening; the words stay in its draft.
  useEffect(() => () => { if (off.current) stop() }, [stop])

  return { available: window.kith.speech.available, listening, start, stop }
}
