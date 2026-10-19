import Foundation

// Compiled with unmodified Commits reader sources by live-replay.py. No app,
// account manager, uploader, or network client is launched by this executable.
@main
struct ReplayDriver {
  static func main() async throws {
    let home = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
    var oracle = LiveUsageOracle(home: home)
    _ = await oracle.snapshot()
    try emit(["type": "ready"])
    while let line = readLine() {
      let command = try JSONSerialization.jsonObject(with: Data(line.utf8)) as! [String: String]
      if command["op"] == "restart" {
        oracle = LiveUsageOracle(home: home)
        _ = await oracle.snapshot()
      }
      let started = DispatchTime.now().uptimeNanoseconds
      let snapshot = await oracle.snapshot(forceScan: true)
      let counters = snapshot?.counters ?? UsageCounters()
      try emit([
        "type": "snapshot",
        "inputTokens": counters.input,
        "outputTokens": counters.output,
        "cacheCreationTokens": counters.cacheWrite,
        "cacheReadTokens": counters.cacheRead,
        "totalTokens": counters.total,
        "snapshotMs": Double(DispatchTime.now().uptimeNanoseconds - started) / 1_000_000,
      ])
    }
  }

  static func emit(_ value: [String: Any]) throws {
    var data = try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys])
    data.append(0x0A)
    try FileHandle.standardOutput.write(contentsOf: data)
  }
}
