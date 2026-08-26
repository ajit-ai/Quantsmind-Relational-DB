# QuantsMind — Homebrew Formula
# To install: brew tap ajit-ai/quantsmind && brew install quantsmind

class Quantsmind < Formula
  desc "Production-grade relational database engine with HTAP support"
  homepage "https://github.com/ajit-ai/Quantsmind-Relational-DB"
  version "0.1.0"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/ajit-ai/Quantsmind-Relational-DB/releases/download/v0.1.0/qmind-macos-arm64-0.1.0.tar.gz"
      sha256 "TO_BE_FILLED_AFTER_RELEASE"
    else
      url "https://github.com/ajit-ai/Quantsmind-Relational-DB/releases/download/v0.1.0/qmind-macos-x64-0.1.0.tar.gz"
      sha256 "TO_BE_FILLED_AFTER_RELEASE"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/ajit-ai/Quantsmind-Relational-DB/releases/download/v0.1.0/qmind-linux-arm64-0.1.0.tar.gz"
      sha256 "TO_BE_FILLED_AFTER_RELEASE"
    else
      url "https://github.com/ajit-ai/Quantsmind-Relational-DB/releases/download/v0.1.0/qmind-linux-x64-0.1.0.tar.gz"
      sha256 "TO_BE_FILLED_AFTER_RELEASE"
    end
  end

  def install
    bin.install "qmind-server"
    bin.install "qmind-cli"
  end

  test do
    system "#{bin}/qmind-server", "--help"
    system "#{bin}/qmind-cli", "--help"
  end
end
