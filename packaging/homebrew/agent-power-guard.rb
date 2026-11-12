class AgentPowerGuard < Formula
  desc "Keep-awake watchdog utility for workstations running autonomous AI agents"
  homepage "https://github.com/Subham-Maity/agent-power-guard"
  version "1.2.6"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/Subham-Maity/agent-power-guard/releases/download/v#{version}/agent-power-guard-macos-aarch64.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    else
      url "https://github.com/Subham-Maity/agent-power-guard/releases/download/v#{version}/agent-power-guard-macos-x86_64.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  on_linux do
    url "https://github.com/Subham-Maity/agent-power-guard/releases/download/v#{version}/agent-power-guard-linux-x86_64.tar.gz"
    sha256 "0000000000000000000000000000000000000000000000000000000000000000"
  end

  def install
    bin.install "agent-power-guard"
  end

  test do
    system "#{bin}/agent-power-guard", "--help"
  end
end

