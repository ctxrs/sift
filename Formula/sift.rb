class Sift < Formula
  desc "Lossless, token-counted compaction of tool output"
  homepage "https://github.com/ctxrs/sift"
  url "https://github.com/ctxrs/sift/archive/refs/tags/v0.3.0.tar.gz"
  sha256 "dbe768258f2229597c674da0ff56b612640724d1b8194fce17b54f8ae0e97488"
  license "MIT"

  depends_on "rust" => :build

  def install
    system "cargo", "install", "--locked", "--path", ".", "--root", prefix
  end

  test do
    assert_match "Sift #{version}", shell_output("#{bin}/sift --version")
    assert_match "Usage:", shell_output("#{bin}/sift --help")
  end
end
