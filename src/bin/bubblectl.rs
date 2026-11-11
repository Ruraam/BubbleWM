//! Small command-line client for BubbleWM's authenticated local IPC.

use anyhow::{Context, Result, bail};
use bubble_ipc::{Request, Response, ResponseData, unix::UnixIpcClient};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Command {
    Capabilities,
    WorkspaceStatus,
    WorkspacePrevious,
    WorkspaceNext,
    ActivateWorkspace(u32),
    Help,
}

fn parse_command(args: &[String]) -> Result<Command> {
    match args {
        [] => Ok(Command::WorkspaceStatus),
        [command] if command == "help" || command == "--help" || command == "-h" => {
            Ok(Command::Help)
        }
        [command] if command == "capabilities" => Ok(Command::Capabilities),
        [command] if command == "workspace" => Ok(Command::WorkspaceStatus),
        [command, action] if command == "workspace" => match action.as_str() {
            "status" | "list" => Ok(Command::WorkspaceStatus),
            "prev" | "previous" => Ok(Command::WorkspacePrevious),
            "next" => Ok(Command::WorkspaceNext),
            _ => bail!("unknown workspace action: {action}"),
        },
        [command, action, index] if command == "workspace" && action == "go" => {
            let number = index
                .parse::<u32>()
                .with_context(|| format!("invalid workspace number: {index}"))?;
            if number == 0 {
                bail!("workspace numbers start at 1");
            }
            Ok(Command::ActivateWorkspace(number - 1))
        }
        _ => bail!("invalid arguments; run `bubblectl help` for usage"),
    }
}

fn request_for(command: Command) -> Option<Request> {
    match command {
        Command::Capabilities => Some(Request::GetCapabilities),
        Command::WorkspaceStatus => Some(Request::GetWorkspaceState),
        Command::WorkspacePrevious => Some(Request::WorkspacePrevious),
        Command::WorkspaceNext => Some(Request::WorkspaceNext),
        Command::ActivateWorkspace(index) => Some(Request::ActivateWorkspace { index }),
        Command::Help => None,
    }
}

fn print_response(client: &mut UnixIpcClient, response: Response) -> Result<()> {
    match response {
        Response::Failure { error } => bail!("{:?}: {}", error.code, error.message),
        Response::Success {
            data: ResponseData::Capabilities { workspace_control },
        } => {
            println!(
                "Workspace control: {}",
                if workspace_control { "enabled" } else { "disabled" }
            );
        }
        Response::Success {
            data: ResponseData::WorkspaceState { active, count },
        } => {
            println!("Active workspace: {} / {count}", active + 1);
        }
        Response::Success {
            data: ResponseData::Acknowledged,
        } => {
            let state = client
                .request(Request::GetWorkspaceState)
                .context("workspace command succeeded, but state refresh failed")?;
            print_response(client, state)?;
        }
    }
    Ok(())
}

fn print_help() {
    println!(
        "BubbleWM control utility

Usage:
  bubblectl [capabilities]
  bubblectl workspace [status|list]
  bubblectl workspace prev
  bubblectl workspace next
  bubblectl workspace go <number>

Workspace numbers are one-based. Commands require a running BubbleWM compositor."
    );
}

fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let command = parse_command(&args)?;
    if command == Command::Help {
        print_help();
        return Ok(());
    }

    let request = request_for(command).expect("non-help commands always have a request");
    let mut client = UnixIpcClient::connect_default()
        .context("cannot locate the BubbleWM IPC socket; is BubbleWM running?")?;
    let response = client
        .request(request)
        .context("BubbleWM IPC request failed")?;
    print_response(&mut client, response)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn workspace_commands_parse_user_facing_one_based_indices() {
        assert_eq!(
            parse_command(&args(&["workspace", "go", "3"])).unwrap(),
            Command::ActivateWorkspace(2)
        );
        assert_eq!(
            parse_command(&args(&["workspace", "prev"])).unwrap(),
            Command::WorkspacePrevious
        );
        assert_eq!(
            parse_command(&args(&["workspace", "next"])).unwrap(),
            Command::WorkspaceNext
        );
    }

    #[test]
    fn workspace_status_is_the_default_command() {
        assert_eq!(parse_command(&[]).unwrap(), Command::WorkspaceStatus);
        assert_eq!(
            parse_command(&args(&["workspace", "list"])).unwrap(),
            Command::WorkspaceStatus
        );
    }

    #[test]
    fn rejects_zero_and_invalid_workspace_indices() {
        assert!(parse_command(&args(&["workspace", "go", "0"])).is_err());
        assert!(parse_command(&args(&["workspace", "go", "abc"])).is_err());
    }
}
