class Turbotokens < Formula
  desc "Real-time token and cost telemetry for AI coding agents"
  homepage "https://github.com/seammoney/turbotokens"
  version "1.1.5"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/seammoney/turbotokens/releases/download/v#{version}/turbotokens-macos-arm64.tar.gz"
      sha256 "b82c23fc88471c9fa43acfffeff34bf7b4e445cf6ddd54f86920711feb703a46"
    end
    on_intel do
      url "https://github.com/seammoney/turbotokens/releases/download/v#{version}/turbotokens-macos-x64.tar.gz"
      sha256 "dbc7d0dfb8acfd64474b27669d8993e025107f6935ce084bca12d0dacc8b6298"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/seammoney/turbotokens/releases/download/v#{version}/turbotokens-linux-arm64.tar.gz"
      sha256 "dfffc9f8208be011f09e60a3e0bfc71f2c2a81e8482d85346c1350eecc81c397"
    end
    on_intel do
      url "https://github.com/seammoney/turbotokens/releases/download/v#{version}/turbotokens-linux-x64.tar.gz"
      sha256 "1e8617cf64b207a0f546db8f158a5f289eb790f100b17f420903039ed3a9032f"
    end
  end

  def install
    bin.install "turbotokens"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/turbotokens --version")
  end
end
