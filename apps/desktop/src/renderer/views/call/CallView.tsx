import { isWorking } from '@shared/models'
import { createContext, useContext, useEffect, useRef, useState } from 'react'
import { Sheet } from '../../components/Overlay'
import { callPresence } from '../../store/call-presence'
import { useModel } from '../../lib/observable'
import { useStore } from '../../store/context'
import { chosenKithVoice, voiceLocalPrepare, voiceLocalSpeak, voiceLocalStatus } from '../settings/voice-settings'
import { VoiceChatSettingsView } from '../settings/VoiceChatSettingsView'
import { CallOperator } from './call-operator'
import { CallScreen } from './CallScreen'

/**
 * Opens the window's own Settings at Voice chat; without it (no Settings screen around) the
 * call bar shows the voice settings in their own sheet.
 */
export const OpenVoiceSettings = createContext<(() => void) | null>(null)
import { CloudSpeechEngine } from './CloudSpeechEngine'
import { GeminiLiveEngine } from './GeminiLiveEngine'
import { KithVoiceEngine } from './KithVoiceEngine'
import { OnDeviceEngine } from './OnDeviceEngine'
import { OpenAIRealtimeEngine } from './OpenAIRealtimeEngine'
import { countVoiceUsage, fromBase64, onDeviceAvailable, onDeviceUnavailableReason, providerName, toBase64, VoiceEngine, VoiceSettings, type VoiceProvider, type VoiceStatus } from './voice'
import { watchVoiceCall } from './voice-call'
import { stopDictation } from '../thread/dictation'
import './call.css'

/**
 * A voice call with a bot: the call screen takes over the chat while it lasts. What you say goes out as ordinary messages, its final replies are read
 * aloud, and the chat gets a "Voice chat" line when the call connects, with its length ("· 00:16") filled in at the end.
 */
export function CallView({ botId, greet, onSpeaking, interrupt, onEnd }: {
  botId: string
  /** An agent's call the person accepted: once the call is live the agent speaks first. */
  greet?: string | null
  onSpeaking: (speaking: boolean) => void
  interrupt: { current: (() => void) | null }
  onEnd: () => void
}) {
  const store = useStore()
  const [session, setSession] = useState<VoiceEngine | null>(null)
  useModel(session)
  /** One line under the bar, e.g. why the call fell back to another voice. */
  const [notice, setNotice] = useState<string | null>(null)
  /** No engine could start at all. */
  const [blocked, setBlocked] = useState<string | null>(null)
  const [settings, setSettings] = useState(false)
  const openVoiceSettings = useContext(OpenVoiceSettings)
  const engine = useRef<VoiceEngine | null>(null)
  /** When the current cloud engine started, for this month's minutes. */
  const cloudStartedAt = useRef<number | null>(null)
  const closed = useRef(false)
  const greeted = useRef(false)
  /** The chat's "Voice chat" line, made once the call connects (null until then, or never). */
  const record = useRef<Promise<string | null> | null>(null)
  const bot = store.bots.get(botId)

  const countCloudMinutes = () => {
    const provider = engine.current?.provider
    if (!provider || cloudStartedAt.current === null) return
    countVoiceUsage((Date.now() - cloudStartedAt.current) / 1000, provider)
    cloudStartedAt.current = null
  }

  const credential = (provider: VoiceProvider, model: string, voice: string) => async () => {
    const client = await store.ready()
    return (await client.call<{ credential: string }>('voiceSession', { provider, model, voice }, 30_000)).credential
  }

  /** Starts the engine: a realtime provider on the key kept by the computer, or on-device speech. */
  const begin = (provider: VoiceProvider | null) => {
    if (closed.current) return
    if (!provider && !onDeviceAvailable()) return void fallbackFromDevice()
    const op = new CallOperator(store, botId)
    const send = (text: string) => store.send(text, botId, null, [], crypto.randomUUID(), [], false, null, 'call')
    let next: VoiceEngine
    if (provider && VoiceSettings.isSpeechMode(provider)) {
      const transcribe = VoiceSettings.transcribeModel(provider)
      const speechModel = VoiceSettings.speechModel(provider)
      const voice = VoiceSettings.voice(provider)
      next = new CloudSpeechEngine(provider, {
        // ponytail: kit turns Simplified Chinese transcripts into Traditional (ICU Hans-Hant); Chromium has no such transform.
        transcribe: async (audio) =>
          (await (await store.ready()).call<{ text: string }>('voiceTranscribe', { provider, model: transcribe, audio: toBase64(audio) }, 60_000)).text,
        speak: async (text) => {
          const clip = await (await store.ready()).call<{ audio: string; format: string }>('voiceSpeak', { provider, model: speechModel, voice, text }, 90_000)
          return { audio: fromBase64(clip.audio), format: clip.format }
        },
        send,
      })
      cloudStartedAt.current = Date.now()
    } else if (provider) {
      const model = VoiceSettings.model(provider)
      const voice = VoiceSettings.voice(provider)
      next =
        provider === 'openai'
          ? new OpenAIRealtimeEngine(credential(provider, model, voice), op)
          : new GeminiLiveEngine(model, voice, credential(provider, model, voice), op)
      cloudStartedAt.current = Date.now()
    } else if (VoiceSettings.isKith()) {
      next = new KithVoiceEngine(send, {
        status: async () => voiceLocalStatus(await store.ready()),
        prepare: async () => voiceLocalPrepare(await store.ready()),
        speak: async (text, voice) => fromBase64((await voiceLocalSpeak(await store.ready(), text, voice)).audio),
        voice: chosenKithVoice,
      })
    } else {
      next = new OnDeviceEngine(send)
    }
    // Picks up the computer's newest models for the next call; this one starts right away.
    if (provider) void rememberStatus()
    let stopWatching: (() => void) | null = null
    next.onActivityChanged = (active) => {
      // The line is written once the call really connects, so a call that never does leaves none.
      if (active && !record.current) record.current = store.startCall(botId)
      if (active && !stopWatching) {
        if (greet && !greeted.current) {
          greeted.current = true
          store.greetCall(greet)
        }
        stopWatching = watchVoiceCall(store, botId, {
          speak: (t) => next.speak(t),
          announce: (t) => next.announce(t),
          stream: next.streamsReplies ? (id, text, final) => next.streamReply(id, text, final) : undefined,
          working: (on) => next.setWorking(on),
        })
      } else if (!active && stopWatching) {
        stopWatching()
        stopWatching = null
      }
    }
    engine.current = next
    setSession(next)
    // Every engine takes the microphone: a recording in a composer ends, its words stay in the draft.
    stopDictation()
    void next.start()
  }

  const rememberStatus = async () => {
    try {
      const status = await (await store.ready()).call<VoiceStatus>('voiceStatus')
      VoiceSettings.remember(status)
      return status
    } catch {
      return null
    }
  }

  /** No on-device speech here (Linux, or a build without the helper): use a provider with a key, if any. */
  const fallbackFromDevice = async () => {
    const configured = (await rememberStatus())?.providers.find((p) => p.configured)?.provider
    if (closed.current) return
    if (configured) {
      setNotice(`${onDeviceUnavailableReason()} Using ${providerName(configured)}.`)
      begin(configured)
    } else {
      setBlocked(`${onDeviceUnavailableReason()} Add an OpenAI or Gemini key in call settings.`)
    }
  }

  useEffect(() => {
    const startedAt = Date.now()
    callPresence.active++
    begin(VoiceSettings.provider())
    return () => {
      closed.current = true
      callPresence.active--
      engine.current?.end()
      countCloudMinutes()
      onSpeaking(false)
      interrupt.current = null
      void store.finishCall(botId, record.current, Math.floor((Date.now() - startedAt) / 1000))
    }
  }, [])

  const phase = session?.phase ?? null
  useEffect(() => {
    if (!session) return
    onSpeaking(phase === 'speaking')
    interrupt.current = phase === 'speaking' ? () => session.interrupt() : null
    // A cloud engine that can't start or drops mid-call hands over to on-device voice.
    if (phase === 'failed' && session.provider && onDeviceAvailable()) {
      countCloudMinutes()
      setNotice(`${providerName(session.provider)} isn't available (${session.failure}). Using on-device voice.`)
      begin(null)
    }
  }, [session, phase])

  const muted = session?.muted === true
  const working = bot ? isWorking(bot) : false
  const downloading = session?.warning?.startsWith('Downloading') === true
  const statusText =
    phase === null || phase === 'starting'
      ? 'Connecting'
      : phase === 'speaking'
        ? 'Speaking'
        : phase === 'failed'
          ? "Can't listen"
          : muted
            ? 'Muted'
            : downloading
              ? 'Downloading voice…'
              : working
                ? 'Working…'
                : 'Listening'
  const failure = blocked ?? (phase === 'failed' ? session?.failure : null) ?? null

  return (
    <>
      <CallScreen
        bot={bot}
        computer={store.computer.name}
        session={session}
        statusText={statusText}
        failure={failure}
        notice={notice}
        working={working}
        onSettings={() => (openVoiceSettings ? openVoiceSettings() : setSettings(true))}
        onEnd={onEnd}
      />
      <Sheet open={settings} onClose={() => setSettings(false)}>
        <VoiceChatSettingsView />
      </Sheet>
    </>
  )
}
