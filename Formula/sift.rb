class Sift < Formula
  desc "Lossless, token-counted compaction of tool output"
  homepage "https://github.com/ctxrs/sift"
  url "https://github.com/ctxrs/sift/archive/refs/tags/v0.4.0.tar.gz"
  sha256 "125d3e9e28dd18236f47e3565c1c15f3510f93b8f496b020005697f653968845"
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
