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
      url "https://github.com/the-cybersapien/lumberroom/releases/download/v0.3.1/lumberroom-0.3.1-aarch64-apple-darwin.tar.gz"
      sha256 "e8d046127b879623c47b7f4d2565b05a617416212848ed546b299503bd4688b3"
    end
    on_intel do
      url "https://github.com/the-cybersapien/lumberroom/releases/download/v0.3.1/lumberroom-0.3.1-x86_64-apple-darwin.tar.gz"
      sha256 "db86e0db0e079dee77fc20b2ae2e029d135e3c0cc26cb19db91f4b541dcd8010"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/the-cybersapien/lumberroom/releases/download/v0.3.1/lumberroom-0.3.1-aarch64-unknown-linux-musl.tar.gz"
      sha256 "7595da2f8193258910635f5942116089437f3e4c5c9d900625bda8068aef5035"
    end
    on_intel do
      url "https://github.com/the-cybersapien/lumberroom/releases/download/v0.3.1/lumberroom-0.3.1-x86_64-unknown-linux-musl.tar.gz"
      sha256 "7fefceb23117742cf9d37400619c59dd825f0063b84c11bb8547d232a8bc0ecc"
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
