use std::{io, path::Path};
use windows_sandbox_spike::{SandboxConfig, run_command};

fn main() -> io::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 3 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Usage: run-command <config.json> <new-workspace-path> <command>",
        ));
    }
    let config: SandboxConfig = serde_json::from_slice(&std::fs::read(&args[0])?)?;
    let exit = run_command(&config, Path::new(&args[1]), &args[2])?;
    println!("Sandbox command exit code: {exit}");
    if exit != 0 {
        return Err(io::Error::other("The sandboxed command failed."));
    }
    Ok(())
}
