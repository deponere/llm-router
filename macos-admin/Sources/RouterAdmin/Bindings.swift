import SwiftUI

// Convenience bindings into the optional config tree. All force-unwrap `config`,
// which is safe because the editor views only render when config != nil.
extension AppState {
    func backend(_ key: String) -> Binding<Backend> {
        Binding(get: { self.config!.backends[key]! },
                set: { self.config!.backends[key] = $0; self.dirty = true })
    }

    func profile(_ key: String) -> Binding<Profile> {
        Binding(get: { self.config!.profiles[key]! },
                set: { self.config!.profiles[key] = $0; self.dirty = true })
    }

    func override(_ i: Int) -> Binding<Override> {
        Binding(get: { self.config!.registry.overrides[i] },
                set: { self.config!.registry.overrides[i] = $0; self.dirty = true })
    }

    var registryIntelligence: Binding<Intelligence> {
        Binding(get: { self.config!.registry.intelligence },
                set: { self.config!.registry.intelligence = $0; self.dirty = true })
    }

    var registryAliases: Binding<[String: String]> {
        Binding(get: { self.config!.registry.intelligence.aliases },
                set: { self.config!.registry.intelligence.aliases = $0; self.dirty = true })
    }

    var privacyLocal: Binding<[String]> {
        Binding(get: { self.config!.registry.privacy.local },
                set: { self.config!.registry.privacy.local = $0; self.dirty = true })
    }

    var privacyZdr: Binding<[String]> {
        Binding(get: { self.config!.registry.privacy.zdr },
                set: { self.config!.registry.privacy.zdr = $0; self.dirty = true })
    }
}
