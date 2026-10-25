//! One module per subcommand, plus the dispatcher.

mod receive;
mod send;

use crate::args::Command;

/// Runs the chosen subcommand.
pub async fn run(command: Command) -> anyhow::Result<()> {
    match command {
        Command::Receive(args) => receive::run(args).await,
        Command::Send(args) => send::run(args).await,
    }
}
