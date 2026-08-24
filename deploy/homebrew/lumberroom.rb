# Formula for the tap the-cybersapien/homebrew-lumberroom. Copy this file into that repository's
# Formula/ directory at release time; brew resolves formula lookups by path inside a tap, not by
# where the source happens to live before that.
class Lumberroom < Formula
  desc "CLI client for lumberroom, a personal memory control plane"
  homepage "https://lumberroom.cloud"
  version "0.1.0"
  license "Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/the-cybersapien/lumberroom/releases/download/v0.1.0/lumberroom-0.1.0-aarch64-apple-darwin.tar.gz"
      # Real value comes from the aarch64-apple-darwin line of this release's SHA256SUMS.
      sha256 "0000000000000000000000000000000000000000000000000000000000aa"
    end
    on_intel do
      url "https://github.com/the-cybersapien/lumberroom/releases/download/v0.1.0/lumberroom-0.1.0-x86_64-apple-darwin.tar.gz"
      # Real value comes from the x86_64-apple-darwin line of this release's SHA256SUMS.
      sha256 "0000000000000000000000000000000000000000000000000000000000bb"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/the-cybersapien/lumberroom/releases/download/v0.1.0/lumberroom-0.1.0-aarch64-unknown-linux-musl.tar.gz"
      # Real value comes from the aarch64-unknown-linux-musl line of this release's SHA256SUMS.
      sha256 "0000000000000000000000000000000000000000000000000000000000cc"
    end
    on_intel do
      url "https://github.com/the-cybersapien/lumberroom/releases/download/v0.1.0/lumberroom-0.1.0-x86_64-unknown-linux-musl.tar.gz"
      # Real value comes from the x86_64-unknown-linux-musl line of this release's SHA256SUMS.
      sha256 "0000000000000000000000000000000000000000000000000000000000dd"
    end
  end

  def install
    bin.install "lumberroom"
    doc.install "README.md"
  end

  def caveats
    <<~EOS
      Point this binary at a running lumberroom server before using it:
        lumberroom doctor

      To wire up Claude Code on this machine (MCP server, SessionStart hook, CLAUDE.md rule),
      use the wiring script from the source repository:
        client/wire-mac.sh --url https://your-lumberroom-host
    EOS
  end

  test do
    # Every real subcommand talks to a server, so there is no offline path that exits zero. An
    # unknown command is the one deterministic, network-free check available: it exercises argument
    # parsing and dispatch and returns a fixed message and exit code without touching the network.
    output = shell_output("#{bin}/lumberroom not-a-real-command 2>&1", 1)
    assert_match "unknown command not-a-real-command", output
    assert_match "doctor", output
  end
end
