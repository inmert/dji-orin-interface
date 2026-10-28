//! Compile the vendored UDT4 sources plus `shim.cpp` into a static library
//! with the system C++ compiler (`CXX`, default `c++`) and `ar`, so the crate
//! needs no build dependencies.

use std::path::{Path, PathBuf};
use std::process::Command;

const SOURCES: &[&str] = &[
    "api", "buffer", "cache", "ccc", "channel", "common", "core", "epoll", "list", "md5", "packet",
    "queue", "window",
];

fn run(cmd: &mut Command) {
    let status = cmd
        .status()
        .unwrap_or_else(|e| panic!("cannot run {cmd:?}: {e}"));
    assert!(status.success(), "{cmd:?} failed: {status}");
}

fn compile(cxx: &str, src: &Path, obj: &Path, include: &Path) {
    println!("cargo:rerun-if-changed={}", src.display());
    run(Command::new(cxx)
        .args(["-O2", "-fPIC", "-std=c++11", "-DLINUX", "-w", "-c"])
        .arg("-I")
        .arg(include)
        .arg(src)
        .arg("-o")
        .arg(obj));
}

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let vendor = manifest.join("vendor/udt4");
    let cxx = std::env::var("CXX").unwrap_or_else(|_| "c++".into());
    println!("cargo:rerun-if-env-changed=CXX");

    let mut objects = Vec::new();
    for name in SOURCES {
        let obj = out.join(format!("{name}.o"));
        compile(&cxx, &vendor.join(format!("{name}.cpp")), &obj, &vendor);
        objects.push(obj);
    }
    let shim = out.join("shim.o");
    compile(&cxx, &manifest.join("shim.cpp"), &shim, &vendor);
    objects.push(shim);

    let lib = out.join("libudt.a");
    let _ = std::fs::remove_file(&lib);
    run(Command::new("ar").arg("crs").arg(&lib).args(&objects));

    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=udt");
    println!("cargo:rustc-link-lib=dylib=stdc++");
    println!("cargo:rustc-link-lib=dylib=pthread");
}
