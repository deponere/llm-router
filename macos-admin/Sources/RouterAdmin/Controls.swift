import SwiftUI

// Small reusable editors for the optional / list-valued config fields.

struct StringListEditor: View {
    let title: String
    @Binding var values: [String]

    private var text: Binding<String> {
        Binding(
            get: { values.joined(separator: "\n") },
            set: { values = $0.split(separator: "\n", omittingEmptySubsequences: true)
                              .map { $0.trimmingCharacters(in: .whitespaces) }
                              .filter { !$0.isEmpty } }
        )
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title).font(.caption).foregroundStyle(.secondary)
            TextEditor(text: text)
                .font(.system(.caption, design: .monospaced))
                .frame(minHeight: 34, maxHeight: 90)
                .overlay(RoundedRectangle(cornerRadius: 4).stroke(.quaternary))
        }
    }
}

struct OptStringField: View {
    let title: String
    @Binding var value: String?
    private var proxy: Binding<String> {
        Binding(get: { value ?? "" },
                set: { value = $0.isEmpty ? nil : $0 })
    }
    var body: some View {
        HStack {
            Text(title).font(.caption).foregroundStyle(.secondary)
            TextField("—", text: proxy).textFieldStyle(.roundedBorder)
        }
    }
}

struct OptDoubleField: View {
    let title: String
    @Binding var value: Double?
    private var proxy: Binding<String> {
        Binding(get: { value.map { String($0) } ?? "" },
                set: { value = Double($0) })
    }
    var body: some View {
        HStack {
            Text(title).font(.caption).foregroundStyle(.secondary)
            TextField("—", text: proxy).textFieldStyle(.roundedBorder).frame(maxWidth: 90)
        }
    }
}

struct OptIntField: View {
    let title: String
    @Binding var value: Int?
    private var proxy: Binding<String> {
        Binding(get: { value.map { String($0) } ?? "" },
                set: { value = Int($0) })
    }
    var body: some View {
        HStack {
            Text(title).font(.caption).foregroundStyle(.secondary)
            TextField("—", text: proxy).textFieldStyle(.roundedBorder).frame(maxWidth: 90)
        }
    }
}

struct OptBoolPicker: View {
    let title: String
    @Binding var value: Bool?
    var body: some View {
        HStack {
            Text(title).font(.caption).foregroundStyle(.secondary)
            Picker("", selection: Binding(
                get: { value == nil ? 0 : (value! ? 1 : 2) },
                set: { value = $0 == 0 ? nil : ($0 == 1) }
            )) {
                Text("—").tag(0); Text("true").tag(1); Text("false").tag(2)
            }
            .pickerStyle(.segmented).labelsHidden().frame(maxWidth: 160)
        }
    }
}
