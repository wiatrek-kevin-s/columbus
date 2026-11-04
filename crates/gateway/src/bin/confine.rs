//! `columbus-confine`: starts a local server or engine plugin confined at
//! level A, B or C. The gateway runs every confined process through it;
//! see the `columbus-confine` crate.

fn main() {
    std::process::exit(columbus_confine::main_cli());
}
