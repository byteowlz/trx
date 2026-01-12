class Trx < Formula
  desc "Minimal git-backed issue tracker"
  homepage "https://github.com/byteowlz/trx"
  version "0.1.0"
  license "MIT"

  on_macos do
    on_intel do
      url "https://github.com/byteowlz/trx/releases/download/v#{version}/trx-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "DARWIN_X86_SHA256"
    end
    on_arm do
      url "https://github.com/byteowlz/trx/releases/download/v#{version}/trx-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "DARWIN_ARM_SHA256"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/byteowlz/trx/releases/download/v#{version}/trx-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "LINUX_X86_SHA256"
    end
    on_arm do
      url "https://github.com/byteowlz/trx/releases/download/v#{version}/trx-v#{version}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "LINUX_ARM_SHA256"
    end
  end

  def install
    bin.install "trx"
    bin.install "trx-tui"
  end

  test do
    system "#{bin}/trx", "--version"
  end
end
