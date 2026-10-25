import AVFoundation
import Speech
import SwiftUI

/// Dictation into the message box with on-device speech: everything heard shows after what was
/// already typed, and it keeps listening through pauses (a recognition that ends is started again
/// quietly) until `stop`. It uses the call's language setting.
@MainActor @Observable
final class Dictation {
    private(set) var listening = false
    @ObservationIgnored private let engine = AVAudioEngine()
    @ObservationIgnored private var recognizer: SFSpeechRecognizer?
    @ObservationIgnored private var request: SFSpeechAudioBufferRecognitionRequest?
    @ObservationIgnored private var task: SFSpeechRecognitionTask?
    @ObservationIgnored private var heard: ((String) -> Void)?
    @ObservationIgnored private var transcript = DictationTranscript()
    /// When the running recognition began: one that fails at once isn't started again.
    @ObservationIgnored private var recognitionStart = Date.distantPast
    /// Bumped per recognition and stop, so a late result from an earlier recognition is dropped.
    @ObservationIgnored private var generation = 0
    /// The one recording: another one, or a call, ends it.
    private static weak var recording: Dictation?

    /// A call takes the microphone; the words heard stay where they were written.
    static func stopRecording() { recording?.stop() }

    /// `said` after what's already in the box, with one space between.
    nonisolated static func join(_ before: String, _ said: String) -> String {
        if before.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { return said }
        if said.isEmpty { return before }
        return before.last?.isWhitespace == true ? before + said : before + " " + said
    }

    /// `heard` gets everything heard since this start.
    func start(heard: @escaping (String) -> Void) async {
        guard !listening, CallPresence.active == 0, await AVAudioApplication.requestRecordPermission(), await Self.authorized() else { return }
        let recognizer = UserDefaults.standard.string(forKey: OnDeviceEngine.languageKey)
            .flatMap { $0.isEmpty ? nil : SFSpeechRecognizer(locale: Locale(identifier: $0)) } ?? SFSpeechRecognizer()
        guard let recognizer, recognizer.isAvailable else { return }
        do {
            let audio = AVAudioSession.sharedInstance()
            // The call's session: a recording tap on a `.record` session fails on some routes.
            try audio.setCategory(.playAndRecord, mode: .default, options: [.defaultToSpeaker, .allowBluetoothHFP, .duckOthers])
            try audio.setActive(true)
        } catch {
            return
        }
        guard (try? MicTap.format(of: engine.inputNode)) != nil else {
            try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
            return
        }
        Self.recording?.stop()
        Self.recording = self
        self.recognizer = recognizer
        self.heard = heard
        transcript = DictationTranscript()
        listening = true
        recognize()
    }

    func stop() {
        generation += 1
        endRecognition()
        heard = nil
        recognizer = nil
        if listening { try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation) }
        listening = false
        if Self.recording === self { Self.recording = nil }
    }

    /// A new recognition on the running session; the transcript goes on from where the last one ended.
    private func recognize() {
        endRecognition()
        guard let recognizer, let format = try? MicTap.format(of: engine.inputNode) else { return stop() }
        generation += 1
        recognitionStart = .now
        let request = SFSpeechAudioBufferRecognitionRequest()
        request.shouldReportPartialResults = true
        request.addsPunctuation = true
        if recognizer.supportsOnDeviceRecognition { request.requiresOnDeviceRecognition = true }
        self.request = request
        engine.inputNode.installTap(onBus: 0, bufferSize: 1024, format: format, block: Self.tap(request))
        engine.prepare()
        do {
            try engine.start()
        } catch {
            return stop()
        }
        task = recognizer.recognitionTask(with: request, resultHandler: Self.results(self, generation: generation))
    }

    private func endRecognition() {
        task?.cancel()
        task = nil
        request?.endAudio()
        request = nil
        engine.inputNode.removeTap(onBus: 0)
        if engine.isRunning { engine.stop() }
    }

    private func recognized(_ text: String, final: Bool, ends: Bool, generation: Int) {
        guard generation == self.generation else { return }
        transcript.heard(text, ends: final || ends)
        heard?(transcript.text)
        // A final ends the recognition (a long pause, its time limit): listen on in a new one.
        if final { recognize() }
    }

    /// The recognition failed (often "no speech" after a long quiet): start another, unless it fails at once.
    private func failed(generation: Int) {
        guard generation == self.generation else { return }
        guard Date.now.timeIntervalSince(recognitionStart) > 1 else { return stop() }
        transcript.restart()
        recognize()
    }

    private nonisolated static func tap(_ request: SFSpeechAudioBufferRecognitionRequest) -> AVAudioNodeTapBlock {
        { buffer, _ in request.append(buffer) }
    }

    private nonisolated static func results(_ dictation: Dictation, generation: Int) -> @Sendable (SFSpeechRecognitionResult?, (any Error)?) -> Void {
        { result, error in
            let text = result?.bestTranscription.formattedString
            let final = result?.isFinal ?? false
            // The recognizer ended a segment (a pause): its next results may start the transcript over.
            let ends = result?.speechRecognitionMetadata != nil
            let failed = error != nil
            Task { @MainActor in
                if let text { dictation.recognized(text, final: final, ends: ends, generation: generation) }
                if failed, !final { dictation.failed(generation: generation) }
            }
        }
    }

    private nonisolated static func authorized() async -> Bool {
        await withCheckedContinuation { done in
            SFSpeechRecognizer.requestAuthorization { done.resume(returning: $0 == .authorized) }
        }
    }
}

/// Everything heard since the mic was tapped. The recognizer reports the words of its current segment
/// (each result replaces the last); a pause, a recognizer that starts its transcript over, or a fresh
/// recognition after one ends begins a new segment, and the earlier ones are kept.
struct DictationTranscript {
    /// The finished segments.
    private var done = ""
    /// The running segment's words so far.
    private var current = ""
    /// The segment just closed by `ends`: a result that repeats it is the same words, not new ones.
    private var closed: String?

    var text: String { Dictation.join(done, current) }

    /// A result: the running segment's words; `ends` when the recognizer says the segment is over (a final, a pause).
    mutating func heard(_ said: String, ends: Bool) {
        var words = said
        if let closed {
            if said.hasPrefix(closed) {
                words = String(said.dropFirst(closed.count).drop(while: \.isWhitespace))
            } else {
                self.closed = nil
            }
        }
        if !current.isEmpty, Self.startsOver(current, words) { done = Dictation.join(done, current) }
        current = words
        if ends {
            done = Dictation.join(done, current)
            current = ""
            closed = said
        }
    }

    /// The recognition ended without a final; a fresh one starts with nothing heard.
    mutating func restart() {
        done = Dictation.join(done, current)
        current = ""
        closed = nil
    }

    /// The recognizer started a new segment without saying so: the result is shorter than the last one and
    /// begins with another letter. A correction of the same words keeps its start or grows.
    private static func startsOver(_ previous: String, _ said: String) -> Bool {
        guard let first = said.first, let was = previous.first, said.count < previous.count else { return false }
        return first.lowercased() != was.lowercased()
    }
}
