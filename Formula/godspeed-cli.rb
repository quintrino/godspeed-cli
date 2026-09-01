class GodspeedCli < Formula
  desc "Rust-based Godspeed CLI"
  homepage "https://github.com/quintrino/godspeed-cli"
  version "0.1.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/quintrino/godspeed-cli/releases/download/v0.1.0/godspeed-cli-v0.1.0-aarch64-apple-darwin.tar.gz"
      sha256 "7992e71436f4a3267b810b8cdc2175bdc573d23d7ab362c5d12de674671f386f"
    else
      url "https://github.com/quintrino/godspeed-cli/releases/download/v0.1.0/godspeed-cli-v0.1.0-x86_64-apple-darwin.tar.gz"
      sha256 "fe3f6c7ff5650178de3d8b6605d2168c572885f7bd7367fe80abd490215c5e71"
    end
  end

  def install
    bin.install "godspeed-cli"
  end

  test do
    system "#{bin}/godspeed-cli", "--version"
  end
end
