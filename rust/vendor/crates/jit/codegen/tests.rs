#![allow(missing_docs, unused_crate_dependencies)]

use evm2::SpecId;
use evm2_jit_backend::OptimizationLevel;
use evm2_jit_codegen::{EvmCompiler, parse_asm};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};
use tester::{ShouldPanic, TestDesc, TestDescAndFn, TestFn, TestName, TestType};
use walkdir::{DirEntry, WalkDir};

fn main() {
    let code = run_tests();
    std::process::exit(code);
}

fn run_tests() -> i32 {
    let args = std::env::args().collect::<Vec<_>>();
    let mut opts = match tester::test::parse_opts(&args) {
        Some(Ok(opts)) => opts,
        Some(Err(msg)) => {
            eprintln!("error: {msg}");
            return 101;
        }
        None => return 0,
    };

    if opts.test_threads.is_none() {
        opts.test_threads = std::thread::available_parallelism().map(|threads| threads.get()).ok();
    }

    let mut tests = Vec::new();
    make_tests(&mut tests);
    tests.sort_by(|a, b| a.desc.name.as_slice().cmp(b.desc.name.as_slice()));

    if opts.list {
        tester::test_main(&args, tests, Some(opts.options));
        return 0;
    }

    match tester::run_tests_console(&opts, tests) {
        Ok(true) => 0,
        Ok(false) => {
            eprintln!("Some tests failed");
            1
        }
        Err(err) => {
            eprintln!("I/O failure during tests: {err}");
            101
        }
    }
}

fn make_tests(tests: &mut Vec<TestDescAndFn>) {
    let config = Arc::new(Config::new());

    let codegen = config.root.join("crates/jit/tests/codegen");
    for entry in collect_tests(&codegen) {
        let config = Arc::clone(&config);
        let path = entry.path().to_path_buf();
        let stripped = path.strip_prefix(config.root).unwrap();
        let name = stripped.display().to_string();
        tests.push(TestDescAndFn {
            desc: TestDesc {
                name: TestName::DynTestName(name),
                allow_fail: false,
                ignore: false,
                should_panic: ShouldPanic::No,
                test_type: TestType::Unknown,
            },
            testfn: TestFn::DynTestFn(Box::new(move || run_test(&config, &path))),
        });
    }
}

fn collect_tests(root: &Path) -> impl Iterator<Item = DirEntry> {
    WalkDir::new(root)
        .sort_by_file_name()
        .into_iter()
        .map(Result::unwrap)
        .filter(|entry| entry.file_type().is_file())
}

fn run_test(config: &Config, path: &Path) {
    let test_name = path.file_stem().unwrap().to_str().unwrap();
    let build_dir = &config.build_base;
    fs::create_dir_all(build_dir).unwrap();

    compile_test(path, build_dir, test_name);

    let input_path = build_dir.join(test_name).join("opt.ll");
    assert!(input_path.exists(), "no optimized LLVM IR produced at {}", input_path.display());

    let mut filecheck = Command::new(config.filecheck.as_deref().unwrap_or("FileCheck".as_ref()));
    filecheck.arg(path).arg("--input-file").arg(&input_path);
    let output = filecheck.output().expect("failed to run FileCheck");
    assert!(
        output.status.success(),
        "FileCheck failed with {}:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn compile_test(path: &Path, build_dir: &Path, test_name: &str) {
    let source = fs::read_to_string(path).unwrap();
    let bytecode = parse_asm(&source).unwrap();

    let mut compiler = EvmCompiler::new_llvm(false).unwrap();
    compiler.set_opt_level(OptimizationLevel::Default);
    compiler.set_module_name(test_name);
    compiler.set_dump_to(Some(build_dir.to_path_buf()));
    unsafe {
        compiler.jit("test", bytecode.as_slice(), SpecId::CANCUN).unwrap();
    }
}

struct Config {
    root: &'static Path,
    build_base: PathBuf,
    filecheck: Option<PathBuf>,
}

impl Config {
    fn new() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let build_base = root.join("target/tester");
        fs::create_dir_all(&build_base).unwrap();
        Self { root, build_base, filecheck: None }
    }
}
