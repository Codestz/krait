# This file is the seed for the homebrew-krait tap.
# It is auto-updated by the release workflow on every stable tag.
# Tap repo: https://github.com/Codestz/homebrew-krait
#
# Manual install:
#   brew tap Codestz/krait
#   brew install krait

class Krait < Formula
  desc "Code intelligence CLI for AI agents — LSP-backed symbol search and semantic editing"
  homepage "https://github.com/Codestz/krait"
  version "0.1.0"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/Codestz/krait/releases/download/v#{version}/krait-aarch64-apple-darwin.tar.gz"
      sha256 "FILL_IN_AFTER_FIRST_RELEASE"
    else
      url "https://github.com/Codestz/krait/releases/download/v#{version}/krait-x86_64-apple-darwin.tar.gz"
      sha256 "FILL_IN_AFTER_FIRST_RELEASE"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/Codestz/krait/releases/download/v#{version}/krait-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "FILL_IN_AFTER_FIRST_RELEASE"
    else
      url "https://github.com/Codestz/krait/releases/download/v#{version}/krait-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "FILL_IN_AFTER_FIRST_RELEASE"
    end
  end

  def install
    bin.install "krait"
  end

  def caveats
    <<~EOS
      krait auto-installs language servers on first use.
      Run `krait init` in your project to generate a workspace config.

      TypeScript: npm install -g @vtsls/language-server
      Go:         go install golang.org/x/tools/gopls@latest
      Rust:       rustup component add rust-analyzer
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/krait --version")
  end
end
