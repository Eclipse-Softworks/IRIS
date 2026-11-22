# typed: false
# frozen_string_literal: true

class Iris < Formula
  desc "Compiled systems, AI/ML, and UI programming language (compiler, LSP, DAP, REPL)"
  homepage "https://github.com/Eclipse-Softworks/IRIS"
  version "1.0.0-rc3"
  license "Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/Eclipse-Softworks/IRIS/releases/download/v1.0.0-rc3/iris-v1.0.0-rc3-aarch64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
    on_intel do
      url "https://github.com/Eclipse-Softworks/IRIS/releases/download/v1.0.0-rc3/iris-v1.0.0-rc3-x86_64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Eclipse-Softworks/IRIS/releases/download/v1.0.0-rc3/iris-v1.0.0-rc3-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
    on_intel do
      url "https://github.com/Eclipse-Softworks/IRIS/releases/download/v1.0.0-rc3/iris-v1.0.0-rc3-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  def install
    bin.install "bin/iris"
    ui_libs = Dir["bin/libiris_ui_host.*"]
    lib.install ui_libs unless ui_libs.empty?
    if Dir.exist?("toolchain")
      (prefix/"toolchain").install Dir["toolchain/*"]
    end
    (pkgshare).install "LLVM_VERSION" if File.exist?("LLVM_VERSION")
    (pkgshare).install "share/iris/iris.png" if File.exist?("share/iris/iris.png")
    (pkgshare/"examples").install Dir["examples/*"] if Dir.exist?("examples")
    generate_completions_from_executable(bin/"iris", "completions")
  end

  test do
    assert_match "iris ", shell_output("#{bin}/iris --version")
    (testpath/"hello.iris").write <<~EOS
      def main() -> i64 {
        return 0
      }
    EOS
    system bin/"iris", "check", testpath/"hello.iris"
  end
end
