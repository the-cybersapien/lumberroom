# The published copy of this file lives at Formula/lumberroom.rb in the tap
# the-cybersapien/homebrew-lumberroom, and that copy is what brew installs. This one is the staging
# copy: prepare a version here alongside the release it targets, then copy it across. brew resolves
# formula lookups by path inside a tap, not by where the source happens to live before that.
class Lumberroom < Formula
  desc "CLI client for lumberroom, a personal memory control plane"
  homepage "https://lumberroom.cloud"
  license "Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/the-cybersapien/lumberroom/releases/download/v0.1.0/lumberroom-0.1.0-aarch64-apple-darwin.tar.gz"
      sha256 "d3cd2831484481849e561a2d28a6d212a34a64e3ea13cb1eaa129a122466b331"
    end
    on_intel do
      url "https://github.com/the-cybersapien/lumberroom/releases/download/v0.1.0/lumberroom-0.1.0-x86_64-apple-darwin.tar.gz"
      sha256 "59e7ddc7056b393193982feeaa1b7238c0da99215ed205fdd25232c9feab11bf"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/the-cybersapien/lumberroom/releases/download/v0.1.0/lumberroom-0.1.0-aarch64-unknown-linux-musl.tar.gz"
      sha256 "8cab66938cc92a6326face27900fb87aec483a3ed9289c8b29d35714e5cf38c6"
    end
    on_intel do
      url "https://github.com/the-cybersapien/lumberroom/releases/download/v0.1.0/lumberroom-0.1.0-x86_64-unknown-linux-musl.tar.gz"
      sha256 "bec16d2c380fa5be403d7d6a9b96e833750d0095e36a0817cd5506f3ddf2a8ac"
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
    # Both checks run offline. `version` is the one subcommand that answers without a server and
    # exits zero, which is why it exists as a command and not only as a flag. The unknown command
    # covers the other half: argument parsing and dispatch reaching a fixed message and exit 1.
    assert_match "lumberroom #{version}", shell_output("#{bin}/lumberroom version")

    output = shell_output("#{bin}/lumberroom not-a-real-command 2>&1", 1)
    assert_match "unknown command not-a-real-command", output
    assert_match "doctor", output
  end
end
