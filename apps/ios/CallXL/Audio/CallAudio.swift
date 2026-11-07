import AVFoundation
import os

/// The subset of `AVAudioSession` the call audio setup uses (tests pass a recorder).
protocol AudioSessionConfiguring: AnyObject {
    func setCategory(_ category: AVAudioSession.Category, mode: AVAudioSession.Mode, options: AVAudioSession.CategoryOptions) throws
    func setActive(_ active: Bool, options: AVAudioSession.SetActiveOptions) throws
}

extension AVAudioSession: AudioSessionConfiguring {}

/// CLAUDE.md §8: `.playAndRecord` + `.voiceChat`, Bluetooth and speaker allowed, speaker by
/// default. libwebrtc's own iOS audio device module does the capture/playout (§3).
enum CallAudio {
    static let category: AVAudioSession.Category = .playAndRecord
    static let mode: AVAudioSession.Mode = .voiceChat
    static let options: AVAudioSession.CategoryOptions = [.allowBluetooth, .allowBluetoothA2DP, .defaultToSpeaker]

    static func configure(_ session: AudioSessionConfiguring) throws {
        try session.setCategory(category, mode: mode, options: options)
    }
}

/// Reaches libwebrtc's `RTCAudioSession` / `RTCAudioSessionConfiguration` (compiled into
/// CallXLCore.framework) through the Objective-C runtime. The classes are registered when
/// the framework loads (linked with `-ObjC`) but their symbols are not exported, so they
/// are looked up by name instead of linked. This gives the CallKit integration WebRTC
/// expects: manual audio, enabled only while CallKit has activated the session.
enum WebRTCAudioBridge {
    /// Class objects as `AnyObject`, so class methods are sent like instance methods.
    static var sessionClass: AnyObject? {
        NSClassFromString("RTCAudioSession").map { $0 as AnyObject }
    }

    static var configurationClass: AnyObject? {
        NSClassFromString("RTCAudioSessionConfiguration").map { $0 as AnyObject }
    }

    static var shared: NSObject? {
        guard let cls = sessionClass, cls.responds(to: NSSelectorFromString("sharedInstance")) else { return nil }
        return cls.perform(NSSelectorFromString("sharedInstance"))?.takeUnretainedValue() as? NSObject
    }

    /// WebRTC's own configuration, applied whenever its ADM configures the session.
    static var webRTCConfiguration: NSObject? {
        guard let cls = configurationClass, cls.responds(to: NSSelectorFromString("webRTCConfiguration")) else { return nil }
        return cls.perform(NSSelectorFromString("webRTCConfiguration"))?.takeUnretainedValue() as? NSObject
    }

    /// Makes WebRTC configure the session exactly as §8 says (it would otherwise drop
    /// `.defaultToSpeaker` when its ADM starts). Returns false if the classes are missing.
    @discardableResult
    static func applyCallConfiguration() -> Bool {
        guard let cls = configurationClass, let cfg = webRTCConfiguration else { return false }
        cfg.setValue(CallAudio.category.rawValue, forKey: "category")
        cfg.setValue(CallAudio.mode.rawValue, forKey: "mode")
        cfg.setValue(NSNumber(value: CallAudio.options.rawValue), forKey: "categoryOptions")
        let setter = NSSelectorFromString("setWebRTCConfiguration:")
        if cls.responds(to: setter) {
            _ = cls.perform(setter, with: cfg)
        }
        return true
    }

    static var configuredOptions: AVAudioSession.CategoryOptions? {
        guard let n = webRTCConfiguration?.value(forKey: "categoryOptions") as? NSNumber else { return nil }
        return AVAudioSession.CategoryOptions(rawValue: n.uintValue)
    }

    static func setManualAudio(_ on: Bool) {
        shared?.setValue(on, forKey: "useManualAudio")
    }

    static func setAudioEnabled(_ on: Bool) {
        shared?.setValue(on, forKey: "isAudioEnabled")
    }

    static var audioEnabled: Bool {
        (shared?.value(forKey: "isAudioEnabled") as? Bool) ?? false
    }

    static func didActivate(_ session: AVAudioSession) {
        let sel = NSSelectorFromString("audioSessionDidActivate:")
        if let s = shared, s.responds(to: sel) {
            _ = s.perform(sel, with: session)
        }
    }

    static func didDeactivate(_ session: AVAudioSession) {
        let sel = NSSelectorFromString("audioSessionDidDeactivate:")
        if let s = shared, s.responds(to: sel) {
            _ = s.perform(sel, with: session)
        }
    }
}

/// Audio session lifecycle around a call: configured before joining, activated by CallKit
/// (or by us when CallKit is unavailable), deactivated after leaving.
@MainActor
final class CallAudioController {
    private let log = Logger(subsystem: "com.callxl.app", category: "audio")
    private let makeSession: () -> AudioSessionConfiguring
    /// Created on first use: `AVAudioSession.sharedInstance()` costs launch time.
    private lazy var session: AudioSessionConfiguring = makeSession()
    private var activatedByUs = false

    init(session: @escaping @autoclosure () -> AudioSessionConfiguring = AVAudioSession.sharedInstance()) {
        makeSession = session
    }

    /// At launch: WebRTC uses §8's configuration, and with CallKit it waits for the
    /// system to activate the session (manual audio).
    func prepareWebRTC(manualAudio: Bool) {
        if !WebRTCAudioBridge.applyCallConfiguration() {
            log.error("RTCAudioSession not found in CallXLCore; WebRTC keeps its default session setup")
        }
        WebRTCAudioBridge.setManualAudio(manualAudio)
    }

    /// Before `join_session`.
    func configureForCall() {
        do {
            try CallAudio.configure(session)
        } catch {
            log.error("audio session configuration failed: \(error.localizedDescription, privacy: .public)")
        }
    }

    func callKitActivated(_ s: AVAudioSession) {
        WebRTCAudioBridge.didActivate(s)
        WebRTCAudioBridge.setAudioEnabled(true)
    }

    func callKitDeactivated(_ s: AVAudioSession) {
        WebRTCAudioBridge.didDeactivate(s)
        WebRTCAudioBridge.setAudioEnabled(false)
    }

    func activateWithoutCallKit() {
        do {
            try session.setActive(true, options: [])
            activatedByUs = true
        } catch {
            log.error("audio session activation failed: \(error.localizedDescription, privacy: .public)")
        }
        WebRTCAudioBridge.setAudioEnabled(true)
    }

    func callEnded(callKit: Bool) {
        if !callKit || activatedByUs {
            WebRTCAudioBridge.setAudioEnabled(false)
        }
        if activatedByUs {
            try? session.setActive(false, options: .notifyOthersOnDeactivation)
            activatedByUs = false
        }
    }
}
