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
      url "https://github.com/the-cybersapien/lumberroom/releases/download/v0.3.0/lumberroom-0.3.0-aarch64-apple-darwin.tar.gz"
      sha256 "ae7d406fc6f217473ce223860708a1b9ee15e48be67da1f7bbe2ac31873e0d8e"
    end
    on_intel do
      url "https://github.com/the-cybersapien/lumberroom/releases/download/v0.3.0/lumberroom-0.3.0-x86_64-apple-darwin.tar.gz"
      sha256 "5ee1ff03f9a6147f90ec66a451344d3d2bb70e10c63d71810330c0230e0ce6dc"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/the-cybersapien/lumberroom/releases/download/v0.3.0/lumberroom-0.3.0-aarch64-unknown-linux-musl.tar.gz"
      sha256 "ae9b42a2c21cfdaa5f723073c640144353b5b98f1d30b75caa2a3785506d6f5a"
    end
    on_intel do
      url "https://github.com/the-cybersapien/lumberroom/releases/download/v0.3.0/lumberroom-0.3.0-x86_64-unknown-linux-musl.tar.gz"
      sha256 "ef69ccabb457fa7ff35d3de15ccfbe3e394761713330ffd38468a96df55ff803"
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
