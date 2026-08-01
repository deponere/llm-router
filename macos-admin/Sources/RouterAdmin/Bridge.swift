import Foundation
import SwiftUI

enum RouterStatus {
    case running, stopped, unknown
}

// MARK: - Transaction snapshot (`GET /v1/transactions`)

struct TxTotals: Codable {
    let cost_usd: Double
    let count: Int
    let tokens_out: Int
    let tokens_per_sec: Double
}

struct Txn: Codable, Identifiable {
    let unix_ts: Int
    let api: String
    let profile: String
    let backend: String
    let model_id: String
    let duration_ms: Int
    let cost_usd: Double?
    let tokens_out: Int

    var id: String { "\(unix_ts)-\(backend)-\(model_id)-\(duration_ms)" }

    private static let hhmmss: DateFormatter = {
        let f = DateFormatter()
        f.dateFormat = "HH:mm:ss"
        return f
    }()

    var timeString: String {
        Self.hhmmss.string(from: Date(timeIntervalSince1970: TimeInterval(unix_ts)))
    }
}

struct TxSnapshot: Codable {
    let totals_session: TxTotals
    let totals_today_utc: TxTotals
    let recent: [Txn]
}

@MainActor
final class AppState: ObservableObject {
    @Published var config: RouterConfig?
    @Published var status: RouterStatus = .unknown
    @Published var snapshot: TxSnapshot?
    @Published var message: String = ""
    @Published var dirty = false
    @Published var rootPath: String {
        didSet { UserDefaults.standard.set(rootPath, forKey: "rootPath") }
    }

    private var routerProcess: Process?

    init() {
        self.rootPath = UserDefaults.standard.string(forKey: "rootPath")
            ?? "/Users/map/dev/router"
    }

    // MARK: - Paths

    var configPath: String { rootPath + "/config/router.toml" }

    /// Prefer a release build, fall back to debug.
    private func binary(_ name: String) -> String? {
        for variant in ["release", "debug"] {
            let p = "\(rootPath)/target/\(variant)/\(name)"
            if FileManager.default.isExecutableFile(atPath: p) { return p }
        }
        return nil
    }

    var bindAddress: String { config?.server.bind ?? "127.0.0.1:4123" }

    // MARK: - Config load / save

    func load() {
        guard let admin = binary("router-admin") else {
            message = "router-admin binary fehlt — bitte `cargo build --release -p router-admin`."
            return
        }
        do {
            let (out, err, code) = try run(admin, ["dump", configPath])
            guard code == 0 else { message = "dump fehlgeschlagen: \(err)"; return }
            config = try JSONDecoder().decode(RouterConfig.self, from: Data(out.utf8))
            dirty = false
            message = "Config geladen."
        } catch {
            message = "Laden fehlgeschlagen: \(error.localizedDescription)"
        }
    }

    func save() {
        guard let admin = binary("router-admin") else {
            message = "router-admin binary fehlt."; return
        }
        guard let config else { return }
        do {
            let enc = JSONEncoder()
            let data = try enc.encode(config)
            let json = String(decoding: data, as: UTF8.self)
            let (_, err, code) = try run(admin, ["apply", configPath], stdin: json)
            guard code == 0 else { message = "Speichern fehlgeschlagen: \(err)"; return }
            dirty = false
            message = "Gespeichert (Backup: router.toml.bak). Für Wirkung: Router neu starten."
        } catch {
            message = "Speichern fehlgeschlagen: \(error.localizedDescription)"
        }
    }

    // MARK: - Router lifecycle

    func startRouter() {
        guard let router = binary("router") else {
            message = "router binary fehlt — `cargo build --release -p router-api`."
            return
        }
        let p = Process()
        p.executableURL = URL(fileURLWithPath: router)
        p.currentDirectoryURL = URL(fileURLWithPath: rootPath)   // dotenvy liest .env aus cwd
        do {
            try p.run()
            routerProcess = p
            message = "Router gestartet."
        } catch {
            message = "Start fehlgeschlagen: \(error.localizedDescription)"
        }
    }

    func stopRouter() {
        if let p = routerProcess, p.isRunning {
            p.terminate()
            routerProcess = nil
        }
        // Auch verwaiste Router treffen — Pattern endet auf /router, matcht also nicht router-admin.
        _ = try? run("/usr/bin/pkill", ["-f", "target/(release|debug)/router$"])
        message = "Router gestoppt."
    }

    func restartRouter() {
        stopRouter()
        // kurz warten, damit der Port frei wird
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.6) { [weak self] in
            self?.startRouter()
        }
    }

    // MARK: - Health

    func refreshStatus() {
        let host = bindAddress.replacingOccurrences(of: "0.0.0.0", with: "127.0.0.1")
        guard let statusURL = URL(string: "http://\(host)/v1/models"),
              let txURL = URL(string: "http://\(host)/v1/transactions?limit=50") else { return }
        var req = URLRequest(url: statusURL)
        req.timeoutInterval = 1.5
        Task {
            do {
                let (_, resp) = try await URLSession.shared.data(for: req)
                let ok = (resp as? HTTPURLResponse)?.statusCode == 200
                await MainActor.run { self.status = ok ? .running : .stopped }
            } catch {
                await MainActor.run { self.status = .stopped; self.snapshot = nil }
                return
            }
            // Transaktions-Snapshot separat holen; ein Fehler hier lässt den Health-Status unberührt.
            var txReq = URLRequest(url: txURL)
            txReq.timeoutInterval = 1.5
            if let (data, _) = try? await URLSession.shared.data(for: txReq),
               let snap = try? JSONDecoder().decode(TxSnapshot.self, from: data) {
                await MainActor.run { self.snapshot = snap }
            }
        }
    }

    // MARK: - Process helper

    @discardableResult
    private func run(_ exe: String, _ args: [String], stdin: String? = nil)
        throws -> (out: String, err: String, code: Int32)
    {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: exe)
        p.arguments = args
        let outPipe = Pipe(), errPipe = Pipe()
        p.standardOutput = outPipe
        p.standardError = errPipe
        if let stdin {
            let inPipe = Pipe()
            p.standardInput = inPipe
            try p.run()
            inPipe.fileHandleForWriting.write(Data(stdin.utf8))
            inPipe.fileHandleForWriting.closeFile()
        } else {
            try p.run()
        }
        let outData = outPipe.fileHandleForReading.readDataToEndOfFile()
        let errData = errPipe.fileHandleForReading.readDataToEndOfFile()
        p.waitUntilExit()
        return (String(decoding: outData, as: UTF8.self),
                String(decoding: errData, as: UTF8.self),
                p.terminationStatus)
    }
}
