import SwiftUI

/// §9 Settings: audio processing toggles + "Raw mic", bitrate ceiling (default 510 kbps),
/// display name, trusted devices, signaling URL, appearance, download location.
struct SettingsView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    @AppStorage("appearance") private var appearance = Appearance.dark.rawValue
    @State private var name = ""
    @State private var signaling = ""
    @State private var bitrate: Double = 510

    private var settings: Settings {
        model.snapshot.settings
    }

    private func apmBinding(_ key: WritableKeyPath<ApmConfig, Bool>) -> Binding<Bool> {
        Binding(
            get: { settings.apm[keyPath: key] && !settings.rawMic },
            set: { v in
                var cfg = settings.apm
                cfg[keyPath: key] = v
                model.setApm(cfg)
            }
        )
    }

    var body: some View {
        Form {
            Section("You") {
                TextField("Display name", text: $name)
                    .onSubmit { model.setDisplayName(name) }
                    .accessibilityIdentifier("settings.displayName")
            }

            Section {
                Toggle("Raw mic", isOn: Binding(get: { settings.rawMic }, set: { model.setRawMic($0) }))
                    .accessibilityIdentifier("settings.rawMic")
                Toggle("Echo cancellation", isOn: apmBinding(\.aec))
                Toggle("Noise suppression", isOn: apmBinding(\.noiseSuppression))
                if settings.apm.noiseSuppression, !settings.rawMic {
                    Picker("Noise suppression level", selection: Binding(
                        get: { settings.apm.nsLevel },
                        set: { v in
                            var cfg = settings.apm
                            cfg.nsLevel = v
                            model.setApm(cfg)
                        }
                    )) {
                        Text("Low").tag(NsLevel.low)
                        Text("Moderate").tag(NsLevel.moderate)
                        Text("High").tag(NsLevel.high)
                    }
                }
                Toggle("High-pass filter", isOn: apmBinding(\.highPass))
                Toggle("Automatic gain", isOn: apmBinding(\.agc))
            } header: {
                Text("Audio processing")
            } footer: {
                Text("Raw mic turns off all four for the purest sound (use headphones). Changes apply live.")
            }

            Section {
                VStack(alignment: .leading) {
                    HStack {
                        Text("Maximum bitrate")
                        Spacer()
                        Text("\(Int(bitrate)) kbps").foregroundStyle(theme.textMuted)
                    }
                    Slider(value: $bitrate, in: 32 ... 510, step: 2) { editing in
                        if !editing {
                            model.setBitrate(UInt32(bitrate))
                        }
                    }
                    .accessibilityIdentifier("settings.bitrate")
                }
            } footer: {
                Text("CallXL reaches this only when the connection is stable.")
            }

            Section("Audio route") {
                HStack {
                    Text("Output")
                    Spacer()
                    RoutePicker().frame(width: 44, height: 44)
                }
            }

            Section("Trusted devices") {
                if model.snapshot.trustedDevices.isEmpty {
                    Text("None yet").foregroundStyle(theme.textMuted)
                }
                ForEach(model.snapshot.trustedDevices, id: \.key) { d in
                    HStack {
                        Text(d.displayName)
                        Spacer()
                        Text(d.online ? "Online" : "Offline").foregroundStyle(theme.textMuted)
                    }
                    .swipeActions {
                        Button("Forget", role: .destructive) { model.untrust(d) }
                    }
                }
            }

            Section {
                TextField("Signaling URL", text: $signaling)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .keyboardType(.URL)
                    .onSubmit { model.setSignalingUrl(signaling) }
                    .accessibilityIdentifier("settings.signalingUrl")
            } header: {
                Text("Internet")
            } footer: {
                Text("Leave empty for the default. LAN calls never need it.")
            }

            Section("Appearance") {
                Picker("Theme", selection: $appearance) {
                    ForEach(Appearance.allCases) { a in Text(a.label).tag(a.rawValue) }
                }
                .pickerStyle(.segmented)
            }

            Section {
                Text("Files › On My iPhone › CallXL")
                    .accessibilityIdentifier("settings.downloads")
            } header: {
                Text("Received files")
            } footer: {
                Text("Received files are saved in the app's Documents folder, visible in the Files app.")
            }
        }
        .navigationTitle("Settings")
        .scrollContentBackground(.hidden)
        .background(theme.bg.ignoresSafeArea())
        .onAppear {
            name = settings.displayName
            signaling = settings.signalingUrl ?? ""
            bitrate = Double(settings.bitrateCeilingKbps)
        }
        .onDisappear {
            model.setDisplayName(name)
        }
    }
}
