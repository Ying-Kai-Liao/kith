@testable import KithUI
import Testing

/// Dictation keeps everything heard since the mic was tapped, across pauses and fresh recognitions.
@Suite struct DictationTests {
    private func feed(_ results: [(String, Bool)]) -> DictationTranscript {
        var transcript = DictationTranscript()
        for (said, ends) in results { transcript.heard(said, ends: ends) }
        return transcript
    }

    @Test func theRunningSegmentReplacesItselfAsItIsCorrected() {
        #expect(feed([("I", false), ("I scream", false), ("Ice cream is good", false)]).text == "Ice cream is good")
    }

    @Test func aSegmentThatEndsIsKeptWhenTheNextOneStarts() {
        let transcript = feed([("Hello", false), ("Hello world.", true), ("Again", false), ("Again and again", false)])
        #expect(transcript.text == "Hello world. Again and again")
    }

    @Test func aFinalThatRepeatsTheSegmentThePauseEndedAddsNothing() {
        #expect(feed([("Hello world", true), ("Hello world", true)]).text == "Hello world")
    }

    @Test func aRecognizerThatGoesOnAfterASegmentEndKeepsTheWordsOnce() {
        let transcript = feed([("Hello world", true), ("Hello world how", false), ("Hello world how are you", true),
                               ("Hello world how are you fine", false)])
        #expect(transcript.text == "Hello world how are you fine")
    }

    @Test func aTranscriptThatStartsOverWithoutSayingSoKeepsTheEarlierWords() {
        let transcript = feed([("Buy", false), ("Buy milk and bread", false), ("Then", false), ("Then call mom", false)])
        #expect(transcript.text == "Buy milk and bread Then call mom")
    }

    @Test func aRestartKeepsTheRunningWordsAndTheNextRecognitionGoesAfterThem() {
        var transcript = feed([("Buy milk", false)])
        transcript.restart()
        transcript.heard("Buy", ends: false)
        transcript.heard("Buy bread", ends: false)
        #expect(transcript.text == "Buy milk Buy bread")
        transcript.heard("Buy bread.", ends: true)
        transcript.restart()
        transcript.heard("Done", ends: false)
        #expect(transcript.text == "Buy milk Buy bread. Done")
    }

    @Test func dictatedWordsGoAfterTypedTextWithOneSpace() {
        #expect(Dictation.join("Note:", "hello") == "Note: hello")
        #expect(Dictation.join("Note: ", "hello") == "Note: hello")
        #expect(Dictation.join("  ", "hello") == "hello")
        #expect(Dictation.join("Note:", "") == "Note:")
    }

    @Test func whileRecordingTheDockShowsStopAndSend() {
        #expect(ChatDockLayout.trailing(text: "", hasAttachment: false, recording: true, micWithText: true, hasStop: true)
            == [.stopRecording, .send])
        #expect(ChatDockLayout.trailing(text: "hi", hasAttachment: false, recording: false, micWithText: true, hasStop: true)
            == [.mic, .send])
        #expect(ChatDockLayout.trailing(text: "hi", hasAttachment: false, recording: false, micWithText: false, hasStop: false)
            == [.send])
        // The bot's work: its Stop sits beside the mic while the box is empty.
        #expect(ChatDockLayout.trailing(text: "", hasAttachment: false, recording: false, micWithText: true, hasStop: true)
            == [.mic, .stop])
        #expect(ChatDockLayout.trailing(text: "", hasAttachment: true, recording: false, micWithText: true, hasStop: true)
            == [.mic, .send])
        #expect(ChatDockLayout.trailing(text: "", hasAttachment: false, recording: false, micWithText: true, hasStop: false)
            == [.mic])
    }
}
