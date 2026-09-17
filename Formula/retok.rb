class Retok < Formula
  desc "Lossless, token-counted compaction of tool output"
  homepage "https://github.com/ctxrs/retok"
  url "https://github.com/ctxrs/retok/archive/refs/tags/v0.2.0.tar.gz"
  sha256 "190406f58104bcb237e4e416f57707f27f2a3334d79eb5366886f42fdef6e375"
  license "MIT"

  depends_on "rust" => :build

  def install
    system "cargo", "install", "--locked", "--path", ".", "--root", prefix
  end

  test do
    assert_match "Retok #{version}", shell_output("#{bin}/retok --version")
    assert_match "Usage:", shell_output("#{bin}/retok --help")
  end
end
