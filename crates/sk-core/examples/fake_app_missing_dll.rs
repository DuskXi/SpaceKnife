//! A stand-in for the app's program in the swap's tests (src/update/swap.rs) on Windows: it links a
//! DLL that no machine has (as a program built without +crt-static links VCRUNTIME140.dll, which a
//! machine without the Visual C++ Redistributable lacks), so Windows' loader refuses to start it.
//! Elsewhere it does nothing.

#[cfg(windows)]
#[link(name = "spaceknife_missing_runtime", kind = "raw-dylib")]
unsafe extern "system" {
    fn spaceknife_missing_runtime_version() -> u32;
}

fn main() {
    // never true, but the call keeps the import in the program
    #[cfg(windows)]
    if std::env::args().count() > 1000 {
        // SAFETY: never reached; the loader fails before main
        println!("{}", unsafe { spaceknife_missing_runtime_version() });
    }
}
