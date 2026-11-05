//! `spaceknife-cli`: the console binary (see `sk_server::cli`).

fn main() {
    std::process::exit(sk_server::cli::main());
}
