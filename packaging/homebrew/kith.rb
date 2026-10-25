# Homebrew formula for kith. Copy it into the tap repo Ying-Kai-Liao/homebrew-kith as Formula/kith.rb:
#
#   brew install Ying-Kai-Liao/kith/kith
#
# It installs the release tarball (.github/workflows/release.yml): bin/kith and share/kith/{kernel,...}, the layout
# the binary's kernel lookup expects (../share/kith/kernel from its real path). The copy in the repo has
# placeholder sha256 values on purpose: scripts/update-formula.sh fills version and checksums at release time and
# the release workflow attaches the result (kith.rb) to the release. See README.md here.
class Kith < Formula
  desc "Your coding-agent bots and the agents behind them, from one command"
  homepage "https://github.com/Ying-Kai-Liao/kith"
  version "0.1.0"
  license "PolyForm-Noncommercial-1.0.0"

  on_macos do
    on_arm do
      url "https://github.com/Ying-Kai-Liao/kith/releases/download/v#{version}/kith-#{version}-darwin-arm64.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
    on_intel do
      url "https://github.com/Ying-Kai-Liao/kith/releases/download/v#{version}/kith-#{version}-darwin-x86_64.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  depends_on :macos
  depends_on "python@3"

  def install
    bin.install "bin/kith"
    bin.install "bin/kith-tts" if File.exist?("bin/kith-tts")   # local voice helper, Apple silicon builds only
    bin.install_symlink bin/"kith" => "hq"   # the old name keeps working
    (share/"kith").install Dir["share/kith/*"], Dir["share/kith/.claude-plugin"]
  end

  def caveats
    <<~EOS
      The kernel ships with this install (#{opt_pkgshare}/kernel). To set up your kith folder, agents and the
      Claude Code plugin, run the installer once:
        curl -fsSL https://raw.githubusercontent.com/Ying-Kai-Liao/kith/main/scripts/install.sh | sh
      Then `kith serve start` runs the host on this Mac.
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/kith --version")
    assert_predicate share/"kith/kernel/bin/kith", :exist?
    assert_predicate bin/"hq", :exist?
  end
end
