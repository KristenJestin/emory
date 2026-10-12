//! The CUDA build links sherpa-onnx and onnxruntime as shared libraries, which the sherpa-onnx
//! build script copies beside the binaries: the binaries look for them there, whatever the
//! working directory or `LD_LIBRARY_PATH`.

fn main() {
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_CUDA");
    if std::env::var_os("CARGO_FEATURE_CUDA").is_some() {
        println!("cargo:rustc-link-arg-bins=-Wl,-rpath,$ORIGIN");
    }
}
