#![allow(dead_code)] // removed in Task 21

mod ago;
mod model;

fn main() {
    eprintln!("usage: herdr-pr-pulse [workspace|mine] [--fixtures]");
    std::process::exit(2);
}
