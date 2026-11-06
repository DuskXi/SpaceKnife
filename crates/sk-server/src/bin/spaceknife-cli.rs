//! `spaceknife-cli`: the console binary (see `sk_server::cli`).

fn main() {
    // Linux glibc: fixed mmap threshold and at most 4 malloc arenas, before any thread starts
    sk_core::memory::tune_allocator();
    std::process::exit(sk_server::cli::main());
}
