import Foundation

/// UniFFI `EventListener` adapter. `onEvent` is called on a Rust thread: it only hops the
/// event to the main queue (FIFO, so event order is kept) and never blocks (§9, §10).
final class EventBridge: EventListener, @unchecked Sendable {
    private let handler: @MainActor (Event) -> Void

    init(_ handler: @escaping @MainActor (Event) -> Void) {
        self.handler = handler
    }

    func onEvent(event: Event) {
        DispatchQueue.main.async { [handler] in
            MainActor.assumeIsolated {
                handler(event)
            }
        }
    }
}

extension Data {
    /// Lowercase hex (peer keys travel as hex in the core API).
    init?(hex: String) {
        guard hex.count.isMultiple(of: 2) else { return nil }
        var bytes = [UInt8]()
        bytes.reserveCapacity(hex.count / 2)
        var index = hex.startIndex
        while index < hex.endIndex {
            let next = hex.index(index, offsetBy: 2)
            guard let byte = UInt8(hex[index ..< next], radix: 16) else { return nil }
            bytes.append(byte)
            index = next
        }
        self.init(bytes)
    }
}
