import SwiftUI

struct ContentView: View {
    @EnvironmentObject var state: AppState
    @State private var section = 0
    private let statusTimer = Timer.publish(every: 4, on: .main, in: .common).autoconnect()

    var body: some View {
        VStack(spacing: 8) {
            header
            Divider()
            if state.config == nil {
                ContentUnavailableView("Keine Config geladen", systemImage: "doc.questionmark",
                                       description: Text(state.message))
                    .frame(maxHeight: .infinity)
            } else {
                Picker("", selection: $section) {
                    Text("Backends").tag(0)
                    Text("Profile").tag(1)
                    Text("Registry").tag(2)
                    Text("Router").tag(3)
                    Text("Log").tag(4)
                }.pickerStyle(.segmented).labelsHidden()

                ScrollView {
                    switch section {
                    case 0: BackendsSection()
                    case 1: ProfilesSection()
                    case 2: RegistrySection()
                    case 3: RouterSection()
                    default: LogSection()
                    }
                }
            }
            Divider()
            footer
        }
        .padding(10)
        .frame(width: 460, height: 620)
        .onAppear { if state.config == nil { state.load() }; state.refreshStatus() }
        .onReceive(statusTimer) { _ in state.refreshStatus() }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack {
                Circle()
                    .fill(state.status == .running ? .green : (state.status == .stopped ? .red : .gray))
                    .frame(width: 9, height: 9)
                Text("Router").bold()
                Text(state.status == .running ? "läuft" : (state.status == .stopped ? "gestoppt" : "…"))
                    .foregroundStyle(.secondary).font(.caption)
                Text(state.bindAddress).font(.caption).foregroundStyle(.tertiary)
                Spacer()
                if state.dirty {
                    Text("● ungespeichert").font(.caption).foregroundStyle(.orange)
                }
            }
            if let s = state.snapshot?.totals_session {
                HStack(spacing: 10) {
                    Label("\(s.count) Aufrufe", systemImage: "arrow.left.arrow.right")
                    Label(String(format: "%.0f tok/s", s.tokens_per_sec), systemImage: "speedometer")
                    Label("\(s.tokens_out) Tokens", systemImage: "number")
                    if s.cost_usd > 0 {
                        Label(String(format: "$%.4f", s.cost_usd), systemImage: "dollarsign.circle")
                    }
                }
                .font(.caption2).foregroundStyle(.secondary).labelStyle(.titleAndIcon)
            }
        }
    }

    private var footer: some View {
        HStack {
            Text(state.message).font(.caption).foregroundStyle(.secondary)
                .lineLimit(2).frame(maxWidth: .infinity, alignment: .leading)
            Button("Neu laden") { state.load() }
            Button("Speichern") { state.save() }.keyboardShortcut("s").disabled(!state.dirty)
        }
    }
}

// MARK: - Backends

struct BackendsSection: View {
    @EnvironmentObject var state: AppState
    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            ForEach(state.config!.backends.keys.sorted(), id: \.self) { key in
                DisclosureGroup {
                    BackendEditor(binding: state.backend(key))
                } label: {
                    HStack {
                        Toggle("", isOn: state.backend(key).enabled).labelsHidden()
                        Text(key).bold()
                        Text(state.config!.backends[key]!.kind.label)
                            .font(.caption).foregroundStyle(.secondary)
                        if state.config!.backends[key]!.local {
                            Text("lokal").font(.caption2).padding(.horizontal, 4)
                                .background(.blue.opacity(0.2)).clipShape(Capsule())
                        }
                        Spacer()
                        Button(role: .destructive) {
                            state.config!.backends.removeValue(forKey: key); state.dirty = true
                        } label: { Image(systemName: "trash") }.buttonStyle(.borderless)
                    }
                }
                Divider()
            }
            Button {
                var i = 1
                while state.config!.backends["backend\(i)"] != nil { i += 1 }
                state.config!.backends["backend\(i)"] = Backend(
                    enabled: false, kind: .openaiCompat, base_url: "http://localhost:0000/v1",
                    auth: Auth(type: "none", env: nil), local: false)
                state.dirty = true
            } label: { Label("Backend hinzufügen", systemImage: "plus") }
        }
    }
}

struct BackendEditor: View {
    @Binding var binding: Backend
    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Picker("kind", selection: $binding.kind) {
                ForEach(BackendKind.allCases) { Text($0.label).tag($0) }
            }
            HStack { Text("base_url").font(.caption).foregroundStyle(.secondary)
                TextField("", text: $binding.base_url).textFieldStyle(.roundedBorder) }
            Toggle("local (Privacy-Klasse Local, Tie-Break-Bonus)", isOn: $binding.local)
            Picker("auth", selection: Binding(
                get: { binding.auth.type },
                set: { binding.auth.type = $0; if $0 == "none" { binding.auth.env = nil }
                       else if binding.auth.env == nil { binding.auth.env = "API_KEY" } }
            )) { Text("none").tag("none"); Text("api_key").tag("api_key") }
            if binding.auth.type == "api_key" {
                OptStringField(title: "env", value: $binding.auth.env)
            }
            OptStringField(title: "app_referer", value: $binding.app_referer)
            OptStringField(title: "app_title", value: $binding.app_title)
            OptStringField(title: "anthropic_version", value: $binding.anthropic_version)
        }.padding(.leading, 6).padding(.bottom, 4)
    }
}

// MARK: - Profiles

struct ProfilesSection: View {
    @EnvironmentObject var state: AppState
    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            ForEach(state.config!.profiles.keys.sorted(), id: \.self) { key in
                DisclosureGroup {
                    ProfileEditor(binding: state.profile(key))
                } label: {
                    HStack {
                        Text(key).bold()
                        Spacer()
                        Button(role: .destructive) {
                            state.config!.profiles.removeValue(forKey: key); state.dirty = true
                        } label: { Image(systemName: "trash") }.buttonStyle(.borderless)
                    }
                }
                Divider()
            }
            Button {
                var i = 1
                while state.config!.profiles["profile\(i)"] != nil { i += 1 }
                state.config!.profiles["profile\(i)"] = Profile(
                    weights: Weights(cost: 0.25, latency: 0.25, context: 0.25, preference: 0.25, quality: 0),
                    require_privacy_class: [], backend_allowlist: [], preferences: [],
                    model_allowlist: [], model_denylist: [], provider_quantizations: [],
                    provider_only: [], provider_ignore: [])
                state.dirty = true
            } label: { Label("Profil hinzufügen", systemImage: "plus") }
        }
    }
}

struct ProfileEditor: View {
    @Binding var binding: Profile
    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            weightsBlock
            Group {
                OptDoubleField(title: "max_price_out_per_mtok", value: $binding.max_price_out_per_mtok)
                OptDoubleField(title: "max_price_in_per_mtok", value: $binding.max_price_in_per_mtok)
                OptIntField(title: "max_latency_p95_ms", value: $binding.max_latency_p95_ms)
                OptDoubleField(title: "min_intelligence_index", value: $binding.min_intelligence_index)
            }
            StringListEditor(title: "backend_allowlist", values: $binding.backend_allowlist)
            StringListEditor(title: "preferences", values: $binding.preferences)
            StringListEditor(title: "require_privacy_class (Local / Zdr)", values: $binding.require_privacy_class)
            StringListEditor(title: "model_allowlist (glob)", values: $binding.model_allowlist)
            StringListEditor(title: "model_denylist (glob)", values: $binding.model_denylist)
            Group {
                OptStringField(title: "provider_sort (price/latency/throughput)", value: $binding.provider_sort)
                OptBoolPicker(title: "provider_zdr", value: $binding.provider_zdr)
                OptBoolPicker(title: "provider_allow_fallbacks", value: $binding.provider_allow_fallbacks)
                OptBoolPicker(title: "provider_require_parameters", value: $binding.provider_require_parameters)
                OptStringField(title: "provider_data_collection (deny/allow)", value: $binding.provider_data_collection)
            }
            StringListEditor(title: "provider_quantizations", values: $binding.provider_quantizations)
            StringListEditor(title: "provider_only", values: $binding.provider_only)
            StringListEditor(title: "provider_ignore", values: $binding.provider_ignore)
        }.padding(.leading, 6).padding(.bottom, 4)
    }

    private var weightsBlock: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack {
                Text("weights").font(.caption).foregroundStyle(.secondary)
                Spacer()
                Text("Σ \(binding.weights.sum, specifier: "%.2f")")
                    .font(.caption2)
                    .foregroundStyle(abs(binding.weights.sum - 1) < 0.001 ? .green : .orange)
            }
            weightRow("cost", $binding.weights.cost)
            weightRow("latency", $binding.weights.latency)
            weightRow("context", $binding.weights.context)
            weightRow("preference", $binding.weights.preference)
            weightRow("quality", $binding.weights.quality)
        }
    }

    private func weightRow(_ label: String, _ v: Binding<Double>) -> some View {
        HStack {
            Text(label).font(.caption2).frame(width: 74, alignment: .leading)
            Slider(value: v, in: 0...1)
            Text("\(v.wrappedValue, specifier: "%.2f")").font(.caption2).monospaced().frame(width: 34)
        }
    }
}

// MARK: - Registry

struct RegistrySection: View {
    @EnvironmentObject var state: AppState
    var body: some View {
        let intel = state.registryIntelligence
        VStack(alignment: .leading, spacing: 8) {
            Text("Intelligence (Artificial Analysis)").bold().font(.subheadline)
            Toggle("enabled", isOn: intel.enabled)
            HStack { Text("api_key_env").font(.caption).foregroundStyle(.secondary)
                TextField("", text: intel.api_key_env).textFieldStyle(.roundedBorder) }
            HStack { Text("base_url").font(.caption).foregroundStyle(.secondary)
                TextField("", text: intel.base_url).textFieldStyle(.roundedBorder) }
            OptIntField(title: "ttl_seconds", value: Binding(
                get: { state.config!.registry.intelligence.ttl_seconds },
                set: { state.config!.registry.intelligence.ttl_seconds = $0 ?? 86400; state.dirty = true }))
            AliasEditor(aliases: state.registryAliases)

            Divider()
            Text("Privacy-Klassifizierung (OpenRouter-Slugs)").bold().font(.subheadline)
            StringListEditor(title: "local", values: state.privacyLocal)
            StringListEditor(title: "zdr", values: state.privacyZdr)

            Divider()
            Text("Registry-Overrides").bold().font(.subheadline)
            ForEach(state.config!.registry.overrides.indices, id: \.self) { i in
                let o = state.override(i)
                VStack(alignment: .leading, spacing: 3) {
                    HStack {
                        TextField("backend", text: o.backend).textFieldStyle(.roundedBorder).frame(maxWidth: 110)
                        TextField("id_prefix", text: o.id_prefix).textFieldStyle(.roundedBorder)
                        Button(role: .destructive) {
                            state.config!.registry.overrides.remove(at: i); state.dirty = true
                        } label: { Image(systemName: "trash") }.buttonStyle(.borderless)
                    }
                    StringListEditor(title: "input_modalities", values: o.input_modalities)
                    StringListEditor(title: "caps", values: o.caps)
                }
                Divider()
            }
            Button {
                state.config!.registry.overrides.append(
                    Override(backend: "omlx", id_prefix: "", input_modalities: ["text"], caps: []))
                state.dirty = true
            } label: { Label("Override hinzufügen", systemImage: "plus") }
        }
    }
}

struct AliasEditor: View {
    @Binding var aliases: [String: String]
    private var text: Binding<String> {
        Binding(
            get: { aliases.sorted { $0.key < $1.key }.map { "\($0.key) = \($0.value)" }.joined(separator: "\n") },
            set: {
                var m: [String: String] = [:]
                for line in $0.split(separator: "\n") {
                    let parts = line.split(separator: "=", maxSplits: 1)
                    guard parts.count == 2 else { continue }
                    let k = parts[0].trimmingCharacters(in: .whitespaces)
                    let v = parts[1].trimmingCharacters(in: .whitespaces)
                    if !k.isEmpty && !v.isEmpty { m[k] = v }
                }
                aliases = m
            })
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text("aliases (Router-ID = AA-Slug, eine pro Zeile)").font(.caption).foregroundStyle(.secondary)
            TextEditor(text: text)
                .font(.system(.caption, design: .monospaced))
                .frame(minHeight: 50, maxHeight: 120)
                .overlay(RoundedRectangle(cornerRadius: 4).stroke(.quaternary))
        }
    }
}

// MARK: - Router control

struct RouterSection: View {
    @EnvironmentObject var state: AppState
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Button { state.startRouter() } label: { Label("Start", systemImage: "play.fill") }
                Button { state.stopRouter() } label: { Label("Stop", systemImage: "stop.fill") }
                Button { state.restartRouter() } label: { Label("Neustart", systemImage: "arrow.clockwise") }
            }
            Text("Config-Änderungen wirken erst nach einem Neustart (kein Hot-Reload).")
                .font(.caption).foregroundStyle(.secondary)
            Divider()
            VStack(alignment: .leading, spacing: 4) {
                Text("Router-Projektverzeichnis").font(.caption).foregroundStyle(.secondary)
                TextField("", text: $state.rootPath).textFieldStyle(.roundedBorder)
                Text(state.configPath).font(.caption2).foregroundStyle(.tertiary)
            }
            Spacer()
            Button("Router-Admin beenden") { NSApplication.shared.terminate(nil) }
        }.padding(.top, 4)
    }
}

// MARK: - Log

struct LogSection: View {
    @EnvironmentObject var state: AppState
    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if let snap = state.snapshot {
                HStack(spacing: 12) {
                    totalsBox("Session", snap.totals_session)
                    totalsBox("Heute (UTC)", snap.totals_today_utc)
                }
                Divider()
                if snap.recent.isEmpty {
                    Text("Noch keine Aufrufe in dieser Session.")
                        .font(.caption).foregroundStyle(.secondary)
                } else {
                    ForEach(snap.recent) { tx in
                        VStack(alignment: .leading, spacing: 1) {
                            HStack(spacing: 6) {
                                Text(tx.timeString).font(.caption2).monospaced().foregroundStyle(.secondary)
                                Text(tx.api).font(.caption2).padding(.horizontal, 3)
                                    .background(.blue.opacity(0.15)).clipShape(Capsule())
                                Text(tx.model_id).font(.caption).lineLimit(1).truncationMode(.middle)
                                Spacer()
                            }
                            HStack(spacing: 8) {
                                Text(tx.backend).font(.caption2).foregroundStyle(.tertiary)
                                Text("· \(tx.profile)").font(.caption2).foregroundStyle(.tertiary)
                                Spacer()
                                Text("\(tx.tokens_out) tok").font(.caption2).foregroundStyle(.secondary)
                                Text("\(tx.duration_ms) ms").font(.caption2).foregroundStyle(.tertiary)
                                if let c = tx.cost_usd, c > 0 {
                                    Text(String(format: "$%.4f", c)).font(.caption2).foregroundStyle(.tertiary)
                                }
                            }
                        }
                        Divider()
                    }
                }
            } else {
                ContentUnavailableView("Kein Log", systemImage: "list.bullet.rectangle",
                                       description: Text("Router läuft nicht oder hat noch keine Aufrufe verarbeitet."))
            }
        }
    }

    private func totalsBox(_ title: String, _ t: TxTotals) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title).font(.caption2).foregroundStyle(.secondary)
            Text("\(t.count) Aufrufe").font(.caption).bold()
            Text(String(format: "%.0f tok/s · %d tok", t.tokens_per_sec, t.tokens_out))
                .font(.caption2).foregroundStyle(.secondary)
            if t.cost_usd > 0 {
                Text(String(format: "$%.4f", t.cost_usd)).font(.caption2).foregroundStyle(.secondary)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(6)
        .background(.quaternary.opacity(0.4))
        .clipShape(RoundedRectangle(cornerRadius: 6))
    }
}
