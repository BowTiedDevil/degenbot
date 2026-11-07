#![allow(missing_docs)]

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=cpp/lib.cpp");

    // `llvm-sys` (`links = "llvm-23"`) exports the `llvm-config` it links against, so `lib.cpp`
    // compiles against the same headers.
    let llvm_config = std::env::var("DEP_LLVM_23_CONFIG_PATH")
        .expect("DEP_LLVM_23_CONFIG_PATH is not set; `llvm-sys` did not report its llvm-config");

    let output = Command::new(&llvm_config)
        .arg("--cxxflags")
        .output()
        .unwrap_or_else(|error| panic!("failed to run {llvm_config}: {error}"));
    assert!(
        output.status.success(),
        "{llvm_config} --cxxflags failed with {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim(),
    );
    let cxxflags = String::from_utf8(output.stdout)
        .unwrap_or_else(|error| panic!("{llvm_config} --cxxflags is not valid UTF-8: {error}"));

    let mut build = cc::Build::new();
    build.cpp(true);
    for flag in cxxflags.split_whitespace() {
        // Mark LLVM's include dirs as system dirs to silence warnings from its headers only. A
        // distro LLVM reports `-I/usr/include`, which as `-isystem` would precede the C++ standard
        // library's own directories and break `#include_next`.
        match flag.strip_prefix("-I") {
            Some(dir) if dir != "/usr/include" => build.flag("-isystem").flag(dir),
            _ => build.flag(flag),
        };
    }
    build.file("cpp/lib.cpp").compile("evm2_jit_llvm_cpp");
}
