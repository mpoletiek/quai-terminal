//! Terminal prompts. Secrets are read from the controlling terminal (or an explicit
//! file descriptor), never from arguments or ordinary environment variables.

use std::io::{BufRead, IsTerminal, Write};
use wallet_core::{CoreError, Result};
use zeroize::Zeroizing;

/// Whether stdin and stderr are interactive terminals.
pub fn interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

fn stdin_line() -> Result<Zeroizing<String>> {
    let mut line = Zeroizing::new(String::new());
    let n = std::io::stdin().lock().read_line(&mut line).map_err(|e| CoreError::Invalid(format!("could not read input: {e}")))?;
    if n == 0 {
        return Err(CoreError::Rejected("expected input on stdin".into()));
    }
    Ok(Zeroizing::new(line.trim_end_matches(['\n', '\r']).to_string()))
}

/// Read a secret line from the TTY without echo, or one line from piped stdin.
pub fn secret(prompt: &str) -> Result<Zeroizing<String>> {
    if !std::io::stdin().is_terminal() {
        return stdin_line();
    }
    if !interactive() {
        return Err(CoreError::Rejected(format!("{prompt} requires an interactive terminal, piped stdin, or --password-fd")));
    }
    let value = rpassword::prompt_password(format!("{prompt}: ")).map_err(|e| CoreError::Invalid(format!("could not read input: {e}")))?;
    Ok(Zeroizing::new(value))
}

/// Read a secret twice and require a match.
pub fn new_secret(prompt: &str, min_len: usize) -> Result<Zeroizing<String>> {
    if !std::io::stdin().is_terminal() {
        let value = stdin_line()?;
        if value.chars().count() < min_len {
            return Err(CoreError::Invalid(format!("{prompt} must be at least {min_len} characters")));
        }
        return Ok(value);
    }
    loop {
        let first = secret(prompt)?;
        if first.chars().count() < min_len {
            eprintln!("  must be at least {min_len} characters");
            continue;
        }
        let second = secret(&format!("Repeat {}", prompt.to_lowercase()))?;
        if *first == *second {
            return Ok(first);
        }
        eprintln!("  entries did not match; try again");
    }
}

/// Read the wallet password from `--password-fd` or the terminal.
pub fn password(fd: Option<i32>, prompt: &str) -> Result<Zeroizing<String>> {
    if let Some(fd) = fd {
        return read_fd(fd);
    }
    secret(prompt)
}

#[cfg(unix)]
fn read_fd(fd: i32) -> Result<Zeroizing<String>> {
    if fd <= 2 {
        return Err(CoreError::Invalid("--password-fd must not be stdin/stdout/stderr".into()));
    }
    // Opening through /dev/fd avoids unsafe raw-descriptor adoption.
    let path = format!("/dev/fd/{fd}");
    let file = std::fs::File::open(&path).map_err(|e| CoreError::Invalid(format!("cannot read password fd {fd}: {e}")))?;
    let mut line = Zeroizing::new(String::new());
    std::io::BufReader::new(file).read_line(&mut line).map_err(|e| CoreError::Invalid(format!("cannot read password fd {fd}: {e}")))?;
    let trimmed = Zeroizing::new(line.trim_end_matches(['\n', '\r']).to_string());
    Ok(trimmed)
}

#[cfg(not(unix))]
fn read_fd(_fd: i32) -> Result<Zeroizing<String>> {
    Err(CoreError::Invalid("--password-fd is only supported on Unix".into()))
}

/// Visible line input.
pub fn line(prompt: &str) -> Result<String> {
    if !std::io::stdin().is_terminal() {
        return stdin_line().map(|l| l.to_string());
    }
    if !interactive() {
        return Err(CoreError::Rejected(format!("{prompt} requires an interactive terminal")));
    }
    eprint!("{prompt}: ");
    std::io::stderr().flush().ok();
    let mut text = String::new();
    std::io::stdin().lock().read_line(&mut text).map_err(|e| CoreError::Invalid(format!("could not read input: {e}")))?;
    Ok(text.trim().to_string())
}

/// Ask for an explicit confirmation word. `--yes` bypasses; non-interactive without it fails closed.
pub fn confirm(question: &str, word: &str, yes: bool) -> Result<()> {
    if yes {
        return Ok(());
    }
    if !interactive() {
        return Err(CoreError::Rejected("confirmation required: re-run interactively or pass --yes to authorize".into()));
    }
    let answer = line(&format!("{question} Type `{word}` to continue"))?;
    if answer == word { Ok(()) } else { Err(CoreError::Rejected("not confirmed; nothing was signed".into())) }
}
