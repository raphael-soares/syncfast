#![allow(clippy::print_stdout, clippy::print_stderr)]

use clap::Parser;

#[derive(Debug, Parser)]
#[command(version, about = "Keep folders in sync with Google Drive")]
struct Cli {}

fn main() {
    let _ = Cli::parse();
}
