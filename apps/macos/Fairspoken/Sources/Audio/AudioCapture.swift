import AVFoundation
import CoreAudio
import Foundation
import FairspokenCore
import OSLog

/// `AVAudioEngine` input tap → 16 kHz mono Float32 (what Parakeet takes; Int16 only at
/// the host boundary). The engine runs only while recording, so the orange mic
/// indicator is never on between dictations (plan §2.3).
///
/// Everything here is `nonisolated`: the tap block runs on a real-time audio thread and
/// must never hop to (or assert) the main actor.
nonisolated final class AudioCapture: @unchecked Sendable {
    enum CaptureError: LocalizedError {
        case noInput
        case converter
        var errorDescription: String? {
            switch self {
            case .noInput: "No microphone is available."
            case .converter: "This microphone's audio format is not supported."
            }
        }
    }

    static let sampleRate: Double = 16_000
    let meter = LevelMeter()

    private let engine = AVAudioEngine()
    private let lock = NSLock()
    private var samples: [Float] = []
    private var maxSamples = Int(sampleRate) * 600
    private var sink: (@Sendable ([Float]) -> Void)?
    private var converter: AVAudioConverter?
    private var running = false
    private let target = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: AudioCapture.sampleRate, channels: 1, interleaved: false)!
    private static let log = Logger(subsystem: AppInfo.bundleID, category: "audio")

    var isRunning: Bool { lock.withLock { running } }

    /// Starts capture. `sink` receives each converted 16 kHz chunk (remote streaming).
    func start(deviceName: String, maxSeconds: Int, sink: (@Sendable ([Float]) -> Void)?) throws {
        lock.withLock {
            samples.removeAll(keepingCapacity: true)
            samples.reserveCapacity(Int(Self.sampleRate) * 30)
            maxSamples = Int(Self.sampleRate) * max(10, maxSeconds)
            self.sink = sink
        }
        meter.reset()
        let input = engine.inputNode
        input.removeTap(onBus: 0)
        if engine.isRunning { engine.stop() }
        Self.selectDevice(named: deviceName, on: input)

        let format = input.outputFormat(forBus: 0)
        guard format.sampleRate > 0, format.channelCount > 0 else { throw CaptureError.noInput }
        guard let conv = AVAudioConverter(from: format, to: target) else { throw CaptureError.converter }
        conv.downmix = true
        conv.sampleRateConverterQuality = AVAudioQuality.high.rawValue
        converter = conv

        input.installTap(onBus: 0, bufferSize: 1024, format: format) { [weak self] buffer, _ in
            self?.process(buffer)
        }
        engine.prepare()
        try engine.start()
        lock.withLock { running = true }
    }

    /// Stops and returns everything captured (16 kHz mono).
    @discardableResult
    func stop() -> [Float] {
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
        // Flush the converter's internal resampler state into the tail.
        return lock.withLock {
            running = false
            sink = nil
            let out = samples
            samples.removeAll()
            return out
        }
    }

    private func process(_ buffer: AVAudioPCMBuffer) {
        guard let converter else { return }
        let ratio = target.sampleRate / buffer.format.sampleRate
        let capacity = AVAudioFrameCount(Double(buffer.frameLength) * ratio + 64)
        guard let out = AVAudioPCMBuffer(pcmFormat: target, frameCapacity: capacity) else { return }
        let feed = InputFeed(buffer)
        var error: NSError?
        let status = converter.convert(to: out, error: &error) { _, inputStatus in
            guard let next = feed.take() else {
                inputStatus.pointee = .noDataNow
                return nil
            }
            inputStatus.pointee = .haveData
            return next
        }
        guard status != .error, out.frameLength > 0, let channel = out.floatChannelData?[0] else { return }
        let chunk = Array(UnsafeBufferPointer(start: channel, count: Int(out.frameLength)))
        var sum: Float = 0
        for s in chunk { sum += s * s }
        meter.push(rms: (sum / Float(chunk.count)).squareRoot())
        let deliver: (@Sendable ([Float]) -> Void)? = lock.withLock {
            guard running else { return nil }
            let room = maxSamples - samples.count
            if room > 0 { samples.append(contentsOf: chunk.prefix(room)) }
            return sink
        }
        deliver?(chunk)
    }

    /// Hands the tap buffer to the converter exactly once. The converter calls its input
    /// block synchronously on this thread, so the unchecked Sendable is sound.
    private final class InputFeed: @unchecked Sendable {
        private var buffer: AVAudioPCMBuffer?
        init(_ buffer: AVAudioPCMBuffer) { self.buffer = buffer }
        func take() -> AVAudioPCMBuffer? {
            defer { buffer = nil }
            return buffer
        }
    }

    // MARK: Devices

    struct InputDevice: Identifiable, Hashable, Sendable {
        var id: AudioDeviceID
        var name: String
        var uid: String
    }

    /// Input-capable Core Audio devices. Selection is stored by *name* in `audioDevice`
    /// (the Rust app's convention; empty = system default).
    static func inputDevices() -> [InputDevice] {
        var address = AudioObjectPropertyAddress(mSelector: kAudioHardwarePropertyDevices,
                                                 mScope: kAudioObjectPropertyScopeGlobal,
                                                 mElement: kAudioObjectPropertyElementMain)
        var size: UInt32 = 0
        guard AudioObjectGetPropertyDataSize(AudioObjectID(kAudioObjectSystemObject), &address, 0, nil, &size) == noErr else { return [] }
        var ids = [AudioDeviceID](repeating: 0, count: Int(size) / MemoryLayout<AudioDeviceID>.size)
        guard AudioObjectGetPropertyData(AudioObjectID(kAudioObjectSystemObject), &address, 0, nil, &size, &ids) == noErr else { return [] }
        return ids.compactMap { id in
            var streams = AudioObjectPropertyAddress(mSelector: kAudioDevicePropertyStreams,
                                                     mScope: kAudioDevicePropertyScopeInput,
                                                     mElement: kAudioObjectPropertyElementMain)
            var streamSize: UInt32 = 0
            guard AudioObjectGetPropertyDataSize(id, &streams, 0, nil, &streamSize) == noErr, streamSize > 0 else { return nil }
            guard let name = stringProperty(id, kAudioObjectPropertyName), let uid = stringProperty(id, kAudioDevicePropertyDeviceUID) else { return nil }
            return InputDevice(id: id, name: name, uid: uid)
        }
    }

    static func defaultInputName() -> String? {
        var address = AudioObjectPropertyAddress(mSelector: kAudioHardwarePropertyDefaultInputDevice,
                                                 mScope: kAudioObjectPropertyScopeGlobal,
                                                 mElement: kAudioObjectPropertyElementMain)
        var id = AudioDeviceID(0)
        var size = UInt32(MemoryLayout<AudioDeviceID>.size)
        guard AudioObjectGetPropertyData(AudioObjectID(kAudioObjectSystemObject), &address, 0, nil, &size, &id) == noErr else { return nil }
        return stringProperty(id, kAudioObjectPropertyName)
    }

    private static func stringProperty(_ id: AudioObjectID, _ selector: AudioObjectPropertySelector) -> String? {
        var address = AudioObjectPropertyAddress(mSelector: selector, mScope: kAudioObjectPropertyScopeGlobal,
                                                 mElement: kAudioObjectPropertyElementMain)
        var value: Unmanaged<CFString>?
        var size = UInt32(MemoryLayout<Unmanaged<CFString>?>.size)
        guard AudioObjectGetPropertyData(id, &address, 0, nil, &size, &value) == noErr, let value else { return nil }
        return value.takeRetainedValue() as String
    }

    private static func selectDevice(named name: String, on input: AVAudioInputNode) {
        let wanted = name.isEmpty ? nil : inputDevices().first { $0.name == name }
        let target: AudioDeviceID
        if let wanted {
            target = wanted.id
        } else {
            var address = AudioObjectPropertyAddress(mSelector: kAudioHardwarePropertyDefaultInputDevice,
                                                     mScope: kAudioObjectPropertyScopeGlobal,
                                                     mElement: kAudioObjectPropertyElementMain)
            var id = AudioDeviceID(0)
            var size = UInt32(MemoryLayout<AudioDeviceID>.size)
            guard AudioObjectGetPropertyData(AudioObjectID(kAudioObjectSystemObject), &address, 0, nil, &size, &id) == noErr else { return }
            target = id
        }
        guard let unit = input.audioUnit else { return }
        var device = target
        let status = AudioUnitSetProperty(unit, kAudioOutputUnitProperty_CurrentDevice, kAudioUnitScope_Global, 0,
                                          &device, UInt32(MemoryLayout<AudioDeviceID>.size))
        if status != noErr { log.error("Selecting input device failed: \(status)") }
    }
}
