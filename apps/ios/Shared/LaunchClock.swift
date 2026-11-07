import Foundation

/// Milliseconds since this process started (kernel start time), for the §1.5 launch check.
enum LaunchClock {
    private static var marks: [String] = []

    static func msSinceProcessStart() -> Double {
        msSinceProcessStart(at: Date().timeIntervalSince1970)
    }

    static func msSinceProcessStart(at wallSeconds: Double) -> Double {
        var info = kinfo_proc()
        var size = MemoryLayout<kinfo_proc>.stride
        var mib: [Int32] = [CTL_KERN, KERN_PROC, KERN_PROC_PID, getpid()]
        guard sysctl(&mib, 4, &info, &size, nil, 0) == 0 else { return -1 }
        let start = info.kp_proc.p_un.__p_starttime
        let startS = Double(start.tv_sec) + Double(start.tv_usec) / 1_000_000
        return (wallSeconds - startS) * 1000
    }

    /// Records a launch phase (main thread only, launch only). The first call also records
    /// `premain`: when dyld had loaded and initialised every image.
    static func mark(_ name: String) {
        if marks.isEmpty, callxl_launch_premain_s > 0 {
            marks.append("premain=\(Int(msSinceProcessStart(at: callxl_launch_premain_s)))")
        }
        marks.append("\(name)=\(Int(msSinceProcessStart()))")
    }

    static var breakdown: String {
        marks.joined(separator: " ")
    }

    /// `simctl launch` measurement: "<ms to first screen> <phases>" in Documents/launch.txt.
    static func writeReport(_ ms: Double) {
        guard let docs = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first else { return }
        let line = String(format: "%.0f %@\n", ms, breakdown)
        try? line.write(to: docs.appendingPathComponent("launch.txt"), atomically: true, encoding: .utf8)
    }
}
