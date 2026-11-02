//! jn: Janus's command line, a console launcher (src/launcher.cpp, the C++'s jn.com; same idea as Visual Studio's
//! devenv.com). In a terminal, `jn --help` and `jn --version` print to the console and wait; anything else opens
//! the interface (janus, next to it) with the same arguments and returns the prompt at once.
//!
//! The Windows installer puts it alone in bin\ (the folder on PATH, with no DLL in it), janus.exe in the folder
//! above: janus.exe beside it, else there.

use std::process::Command;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let name = if cfg!(windows) { "janus.exe" } else { "janus" };
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| {
            let dir = p.parent()?;
            std::iter::once(dir.join(name)).chain(dir.parent().map(|d| d.join(name))).find(|p| p.is_file())
        })
        .unwrap_or_else(|| name.into());
    // `jn mcp`: the MCP server, on this process's standard input and output (inherited) until they close
    let print = args.iter().any(|a| matches!(a.as_str(), "-h" | "--help" | "/?" | "--version"))
        || args.first().is_some_and(|a| a == "mcp");
    let mut cmd = Command::new(&exe);
    cmd.args(&args);
    if print {
        // the interface prints them to this console (it attaches to it): wait for it
        let rc = cmd.status().map_or(1, |s| s.code().unwrap_or(1));
        std::process::exit(rc);
    }
    if let Err(e) = cmd.spawn() {
        eprintln!("jn: could not start {}: {e}", exe.display());
        std::process::exit(1);
    }
}
