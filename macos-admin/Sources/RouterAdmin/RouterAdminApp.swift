import SwiftUI

struct RouterAdminApp: App {
    @StateObject private var state = AppState()

    var body: some Scene {
        MenuBarExtra {
            ContentView().environmentObject(state)
        } label: {
            // Text label so it's unmistakably visible in the menu bar (a bare SF Symbol can hide behind the notch / be hard to spot).
            Image(systemName: "slider.horizontal.3")
            Text("Router")
        }
        .menuBarExtraStyle(.window)
    }
}

// Custom entry so a headless `--selftest` can exercise the Swift<->Rust config round-trip without launching the menu-bar UI.
@main
struct Entry {
    static func main() {
        if CommandLine.arguments.contains("--selftest") {
            SelfTest.run()
        }
        RouterAdminApp.main()
    }
}
