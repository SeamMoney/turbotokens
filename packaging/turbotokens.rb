class Turbotokens < Formula
  desc "Real-time token and cost telemetry for AI coding agents"
  homepage "https://github.com/seammoney/turbotokens"
  version "1.1.8"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/seammoney/turbotokens/releases/download/v#{version}/turbotokens-macos-arm64.tar.gz"
      sha256 "2a8c78676ac5c5887b2449963b3ee36bf3843aed3a7f3c04249376cdb2730707"
    end
    on_intel do
      url "https://github.com/seammoney/turbotokens/releases/download/v#{version}/turbotokens-macos-x64.tar.gz"
      sha256 "019cf959e5a85496aead8cf2e481bb625c30c8129ad87715e52e4c504c50bd5f"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/seammoney/turbotokens/releases/download/v#{version}/turbotokens-linux-arm64.tar.gz"
      sha256 "73d6e635195b37bf4898c45e0a5534ea784e7b742fc01f2a292544579eb0a3f7"
    end
    on_intel do
      url "https://github.com/seammoney/turbotokens/releases/download/v#{version}/turbotokens-linux-x64.tar.gz"
      sha256 "57004e196fe033bef0c653694ba4945b2a748184a45d54a273422ec5353e77c5"
    end
  end

  def install
    bin.install "turbotokens"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/turbotokens --version")
  end
end
