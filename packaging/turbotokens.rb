class Turbotokens < Formula
  desc "Real-time token and cost telemetry for AI coding agents"
  homepage "https://github.com/seammoney/turbotokens"
  version "1.1.6"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/seammoney/turbotokens/releases/download/v#{version}/turbotokens-macos-arm64.tar.gz"
      sha256 "0a788e255490a0cd5629fa754e9fb6335e9780cd8b4dbfd562f46a0f0853330b"
    end
    on_intel do
      url "https://github.com/seammoney/turbotokens/releases/download/v#{version}/turbotokens-macos-x64.tar.gz"
      sha256 "b15e4e7142eb277976fbc2ff449f9805b6d4fc45d27045e93c9d8d139bfefbae"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/seammoney/turbotokens/releases/download/v#{version}/turbotokens-linux-arm64.tar.gz"
      sha256 "dc67f98edfadadb8abed6268322949220511eb2ceac870eb0371bcb7b6eb7024"
    end
    on_intel do
      url "https://github.com/seammoney/turbotokens/releases/download/v#{version}/turbotokens-linux-x64.tar.gz"
      sha256 "a170551cfaa3046508c5cafc115be848ffe25e0082afd84c5d26d921502aa06c"
    end
  end

  def install
    bin.install "turbotokens"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/turbotokens --version")
  end
end
