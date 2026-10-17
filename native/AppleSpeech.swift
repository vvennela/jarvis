import Foundation
import AVFoundation
import Speech
import Darwin

// One bounded utterance per process. The parent owns deadline/cancellation.
// No microphone is opened; no audio or transcript is written to disk or stderr.
enum InputFailure: Error { case invalid, unavailable, assets, conversion, output }
let maxInputBytes = 1_000_000
let maxFrames = 16_000 * 30

func readInput() throws -> Data {
    var data = Data()
    while let chunk = try FileHandle.standardInput.read(upToCount: min(65536, maxInputBytes + 1 - data.count)), !chunk.isEmpty {
        data.append(chunk)
        if data.count > maxInputBytes { throw InputFailure.invalid }
    }
    return data
}
func pcmBuffer(_ data: Data) throws -> AVAudioPCMBuffer {
    func u16(_ offset: Int) -> UInt16 { UInt16(data[offset]) | UInt16(data[offset + 1]) << 8 }
    func u32(_ offset: Int) -> UInt32 { UInt32(data[offset]) | UInt32(data[offset + 1]) << 8 | UInt32(data[offset + 2]) << 16 | UInt32(data[offset + 3]) << 24 }
    func tag(_ offset: Int, _ text: String) -> Bool { data[offset..<offset + 4].elementsEqual(text.utf8) }
    guard data.count >= 44, tag(0, "RIFF"), tag(8, "WAVE"), Int(u32(4)) + 8 == data.count else { throw InputFailure.invalid }
    var offset = 12
    var validFormat = false
    var samples: Range<Int>?
    while offset + 8 <= data.count {
        let size = Int(u32(offset + 4))
        let begin = offset + 8
        guard size <= data.count - begin else { throw InputFailure.invalid }
        if tag(offset, "fmt ") {
            guard !validFormat, size >= 16, u16(begin) == 1, u16(begin + 2) == 1,
                  u32(begin + 4) == 16000, u32(begin + 8) == 32000,
                  u16(begin + 12) == 2, u16(begin + 14) == 16 else { throw InputFailure.invalid }
            validFormat = true
        } else if tag(offset, "data") {
            guard samples == nil, size > 0, size % 2 == 0, size / 2 <= maxFrames else { throw InputFailure.invalid }
            samples = begin..<begin + size
        }
        offset = begin + size + (size % 2)
    }
    guard validFormat, offset == data.count, let samples,
          let format = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: 16000, channels: 1, interleaved: false),
          let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: AVAudioFrameCount(samples.count / 2)),
          let destination = buffer.floatChannelData?[0] else { throw InputFailure.invalid }
    buffer.frameLength = buffer.frameCapacity
    for frame in 0..<Int(buffer.frameLength) {
        destination[frame] = Float(Int16(bitPattern: u16(samples.lowerBound + frame * 2))) / 32768
    }
    return buffer
}
func convert(_ input: AVAudioPCMBuffer, to format: AVAudioFormat) throws -> AVAudioPCMBuffer {
    if input.format == format { return input }
    let frames = ceil(Double(input.frameLength) * format.sampleRate / input.format.sampleRate) + 256
    guard frames > 0, frames <= 48000 * 31,
          let converter = AVAudioConverter(from: input.format, to: format),
          let output = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: AVAudioFrameCount(frames)) else { throw InputFailure.conversion }
    var supplied = false
    var error: NSError?
    let status = converter.convert(to: output, error: &error) { _, state in
        if supplied { state.pointee = .endOfStream; return nil }
        supplied = true
        state.pointee = .haveData
        return input
    }
    guard status != .error, error == nil, output.frameLength > 0 else { throw InputFailure.conversion }
    return output
}
@available(macOS 26.0, *)
func module() async throws -> SpeechTranscriber {
    guard SpeechTranscriber.isAvailable,
          let locale = await SpeechTranscriber.supportedLocale(equivalentTo: Locale(identifier: "en-US")) else { throw InputFailure.unavailable }
    return SpeechTranscriber(locale: locale, preset: .transcription)
}
@available(macOS 26.0, *)
func transcribe(_ input: AVAudioPCMBuffer) async throws -> String {
    let transcriber = try await module()
    guard await AssetInventory.status(forModules: [transcriber]) == .installed else { throw InputFailure.assets }
    guard let format = await SpeechAnalyzer.bestAvailableAudioFormat(compatibleWith: [transcriber], considering: input.format) else { throw InputFailure.conversion }
    let buffer = try convert(input, to: format)
    let analyzer = SpeechAnalyzer(modules: [transcriber])
    let results = Task { () throws -> String in
        var text = ""
        for try await result in transcriber.results {
            let part = String(result.text.characters)
            guard text.utf8.count + part.utf8.count + 1 <= 8192 else { throw InputFailure.output }
            if !text.isEmpty { text += " " }
            text += part
        }
        return text.trimmingCharacters(in: .whitespacesAndNewlines)
    }
    let stream = AsyncStream<AnalyzerInput> { continuation in
        continuation.yield(AnalyzerInput(buffer: buffer))
        continuation.finish()
    }
    try await analyzer.prepareToAnalyze(in: format)
    try await analyzer.start(inputSequence: stream)
    try await analyzer.finalizeAndFinishThroughEndOfInput()
    return try await results.value
}
@main struct AppleSpeech {
    static func main() async {
        guard #available(macOS 26.0, *) else { exit(3) }
        do {
            if CommandLine.arguments == [CommandLine.arguments[0], "--prepare"] {
                let transcriber = try await module()
                if let installation = try await AssetInventory.assetInstallationRequest(supporting: [transcriber]) {
                    try await installation.downloadAndInstall()
                }
                guard await AssetInventory.status(forModules: [transcriber]) == .installed else { throw InputFailure.assets }
                FileHandle.standardOutput.write(Data("{\"ready\":true}\n".utf8))
                return
            }
            guard CommandLine.arguments.count == 1 else { throw InputFailure.invalid }
            let input = try pcmBuffer(readInput())
            let text = try await transcribe(input)
            guard !text.isEmpty else { throw InputFailure.output }
            let json = try JSONSerialization.data(withJSONObject: ["text": text])
            FileHandle.standardOutput.write(json)
            FileHandle.standardOutput.write(Data([10]))
        } catch InputFailure.assets {
            FileHandle.standardError.write(Data("Apple speech assets are not installed.\n".utf8)); exit(3)
        } catch InputFailure.invalid {
            FileHandle.standardError.write(Data("Invalid bounded PCM WAV input.\n".utf8)); exit(2)
        } catch {
            FileHandle.standardError.write(Data("Local Apple transcription failed.\n".utf8)); exit(1)
        }
    }
}
