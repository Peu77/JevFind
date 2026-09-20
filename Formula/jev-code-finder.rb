class JevCodeFinder < Formula
  desc "Semantic code search powered by TypeSafe Jev"
  homepage "https://github.com/Peu77/JevFind"
  head "https://github.com/Peu77/JevFind.git", branch: "main"
  license "MIT"

  depends_on "rust" => :build

  def install
    system "cargo", "install", "--locked", "--root", prefix
  end

  test do
    assert_match "Find code that matches a concept", shell_output("#{bin}/jev-code-finder --help")
  end
end
