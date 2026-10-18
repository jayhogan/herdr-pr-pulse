#![allow(dead_code)] // removed in Task 21

mod ago;
mod app;
mod attention;
mod model;
mod stacks;
mod ui;

fn main() {
    eprintln!("usage: herdr-pr-pulse [workspace|mine] [--fixtures]");
    std::process::exit(2);
}
