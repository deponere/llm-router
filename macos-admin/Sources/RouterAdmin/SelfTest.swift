import Foundation

// Headless end-to-end check of the Swift<->Rust config bridge:
// dump -> decode -> mutate -> encode -> apply -> dump -> verify.
// Run with: RouterAdmin --selftest <router-admin-binary> <router.toml>
enum SelfTest {
    static func run() {
        let args = CommandLine.arguments
        guard let idx = args.firstIndex(of: "--selftest"),
              args.count > idx + 2 else {
            fail("usage: --selftest <router-admin-binary> <router.toml>")
        }
        let admin = args[idx + 1]
        let source = args[idx + 2]

        // Work on a throwaway copy.
        let tmp = NSTemporaryDirectory() + "router-selftest.toml"
        try? FileManager.default.removeItem(atPath: tmp)
        do { try FileManager.default.copyItem(atPath: source, toPath: tmp) }
        catch { fail("copy failed: \(error)") }

        let before = (try? String(contentsOfFile: tmp, encoding: .utf8)) ?? ""
        let commentsBefore = before.split(separator: "\n").filter { $0.trimmingCharacters(in: .whitespaces).hasPrefix("#") }.count

        // 1. dump -> decode
        var cfg = decode(dump(admin, tmp))

        // 2. mutate a bool, a float weight, and a list
        let omlxWas = cfg.backends["omlx"]?.enabled ?? true
        cfg.backends["omlx"]?.enabled = !omlxWas
        cfg.profiles["default"]?.weights.cost = 0.42
        cfg.profiles["local"]?.preferences.append("selftest-model")

        // 3. encode -> apply
        let json = String(decoding: (try! JSONEncoder().encode(cfg)), as: UTF8.self)
        let applyErr = apply(admin, tmp, stdin: json)
        if !applyErr.isEmpty { fail("apply stderr: \(applyErr)") }

        // 4. dump again -> verify values round-tripped
        let after = decode(dump(admin, tmp))
        check(after.backends["omlx"]?.enabled == !omlxWas, "omlx.enabled toggled")
        check(after.profiles["default"]?.weights.cost == 0.42, "default.weights.cost == 0.42")
        check(after.profiles["local"]?.preferences.contains("selftest-model") == true, "preference appended")

        // 5. comments preserved
        let text = (try? String(contentsOfFile: tmp, encoding: .utf8)) ?? ""
        let commentsAfter = text.split(separator: "\n").filter { $0.trimmingCharacters(in: .whitespaces).hasPrefix("#") }.count
        check(commentsAfter >= commentsBefore && commentsBefore > 0,
              "comments preserved (\(commentsBefore) -> \(commentsAfter))")

        // 6. a weight stayed a float in the TOML (not demoted to int)
        check(text.contains("cost = 0.42"), "cost written as float 0.42")

        print("SELFTEST OK")
        exit(0)
    }

    // MARK: helpers

    static func decode(_ s: String) -> RouterConfig {
        do { return try JSONDecoder().decode(RouterConfig.self, from: Data(s.utf8)) }
        catch { fail("decode failed: \(error)\n---\n\(s.prefix(400))") }
    }

    static func dump(_ admin: String, _ path: String) -> String {
        let (out, err, code) = exec(admin, ["dump", path])
        if code != 0 { fail("dump failed: \(err)") }
        return out
    }

    static func apply(_ admin: String, _ path: String, stdin: String) -> String {
        let (_, err, code) = exec(admin, ["apply", path], stdin: stdin)
        return code == 0 ? "" : (err.isEmpty ? "exit \(code)" : err)
    }

    static func exec(_ exe: String, _ a: [String], stdin: String? = nil) -> (String, String, Int32) {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: exe)
        p.arguments = a
        let o = Pipe(), e = Pipe()
        p.standardOutput = o; p.standardError = e
        if let stdin {
            let i = Pipe(); p.standardInput = i
            try? p.run()
            i.fileHandleForWriting.write(Data(stdin.utf8))
            i.fileHandleForWriting.closeFile()
        } else {
            try? p.run()
        }
        let od = o.fileHandleForReading.readDataToEndOfFile()
        let ed = e.fileHandleForReading.readDataToEndOfFile()
        p.waitUntilExit()
        return (String(decoding: od, as: UTF8.self), String(decoding: ed, as: UTF8.self), p.terminationStatus)
    }

    static func check(_ cond: Bool, _ label: String) {
        if cond { print("  ok  \(label)") } else { fail("CHECK FAILED: \(label)") }
    }

    static func fail(_ msg: String) -> Never {
        FileHandle.standardError.write(Data(("SELFTEST FAIL: " + msg + "\n").utf8))
        exit(1)
    }
}
