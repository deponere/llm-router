import Foundation

// Mirrors router-config's Config schema 1:1. Property names are snake_case on
// purpose: they match the JSON keys emitted by `router-admin dump`, so no
// CodingKeys and no key strategy are needed — and, crucially, the automatic
// .convertToSnakeCase strategy would mangle dictionary keys like the AA aliases
// ("Qwen3.6-35B-A3B-bf16"). Structs are value types; SwiftUI edits them in place.

struct RouterConfig: Codable {
    var server: Server
    var backends: [String: Backend]
    var registry: Registry
    var profiles: [String: Profile]
}

struct Server: Codable {
    var bind: String
}

enum BackendKind: String, Codable, CaseIterable, Identifiable {
    case openaiCompat = "openai_compat"
    case openrouter
    case anthropic
    var id: String { rawValue }
    var label: String {
        switch self {
        case .openaiCompat: return "openai_compat"
        case .openrouter: return "openrouter"
        case .anthropic: return "anthropic"
        }
    }
}

struct Backend: Codable {
    var enabled: Bool
    var kind: BackendKind
    var base_url: String
    var auth: Auth
    var local: Bool
    var app_referer: String?
    var app_title: String?
    var anthropic_version: String?
}

// Internally tagged enum in Rust: {type:"none"} | {type:"api_key", env:"..."}.
struct Auth: Codable {
    var type: String            // "none" | "api_key"
    var env: String?
}

struct Registry: Codable {
    var overrides: [Override]
    var privacy: Privacy
    var intelligence: Intelligence
}

struct Override: Codable, Identifiable {
    var backend: String
    var id_prefix: String
    var input_modalities: [String]
    var caps: [String]
    var id: String { backend + "/" + id_prefix }
}

struct Privacy: Codable {
    var local: [String]
    var zdr: [String]
}

struct Intelligence: Codable {
    var enabled: Bool
    var api_key_env: String
    var base_url: String
    var ttl_seconds: Int
    var aliases: [String: String]
}

struct Profile: Codable {
    var weights: Weights
    var max_price_out_per_mtok: Double?
    var max_price_in_per_mtok: Double?
    var max_latency_p95_ms: Int?
    var require_privacy_class: [String]
    var backend_allowlist: [String]
    var preferences: [String]
    var model_allowlist: [String]
    var model_denylist: [String]
    var provider_sort: String?
    var provider_zdr: Bool?
    var provider_allow_fallbacks: Bool?
    var provider_require_parameters: Bool?
    var provider_data_collection: String?
    var provider_quantizations: [String]
    var provider_only: [String]
    var provider_ignore: [String]
    var min_intelligence_index: Double?
}

struct Weights: Codable {
    var cost: Double
    var latency: Double
    var context: Double
    var preference: Double
    var quality: Double

    var sum: Double { cost + latency + context + preference + quality }
}
