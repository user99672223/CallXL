import AVKit
import CallKit
import Charts
import SwiftUI

/// Launch-time attribution only: references the system frameworks CallXL's UI links
/// (Charts, CallKit, AVKit) so this baseline variant loads them like CallXL does.
enum FrameworkUses {
    static func touch() -> [Any.Type] {
        [CXProviderConfiguration.self, AVRoutePickerView.self, Chart<BarMark>.self]
    }
}
