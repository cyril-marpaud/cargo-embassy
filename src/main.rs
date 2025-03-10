pub mod chip;
pub mod error;
mod init;
pub mod parser;

use clap::Parser;
use init::Init;
use parser::{Cargo, EmbassyCommand};

fn main() {
    let Cargo::Embassy(embassy) = Cargo::parse();

    match embassy.command {
        EmbassyCommand::Init(args) => {
            let init = Init::new();
            init.run(args);
        }
        EmbassyCommand::Docs => open::that("https://embassy.dev/book/dev/index.html")
            .expect("Failed to open Embassy documentation page."),
    }
}
