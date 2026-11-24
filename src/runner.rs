//! Exécution de commandes shell pour l'apprenant (`/run`) et pour la
//! vérification automatique des exercices.

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::process::Command;

const MAX_OUTPUT_LINES: usize = 120;
const MAX_OUTPUT_CHARS: usize = 8000;

#[derive(Debug, Clone)]
pub struct RunOutput {
    pub code: Option<i32>,
    pub output: String,
    pub timed_out: bool,
    pub secs: f32,
}

impl RunOutput {
    pub fn ok(&self) -> bool {
        self.code == Some(0) && !self.timed_out
    }
}

fn shell(cmd: &str) -> Command {
    #[cfg(windows)]
    {
        let mut c = Command::new("cmd");
        c.arg("/D").arg("/C").raw_arg(cmd);
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = Command::new("sh");
        c.arg("-c").arg(cmd);
        c
    }
}

pub async fn run(cmd: &str, cwd: &Path, timeout: Duration) -> RunOutput {
    let started = Instant::now();
    let child = shell(cmd)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("NO_COLOR", "1")
        .env("CARGO_TERM_COLOR", "never")
        .env("FORCE_COLOR", "0")
        .kill_on_drop(true)
        .spawn();
    let child = match child {
        Ok(c) => c,
        Err(e) => {
            return RunOutput { code: None, output: e.to_string(), timed_out: false, secs: 0.0 };
        }
    };
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(out)) => {
            let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
            let err = String::from_utf8_lossy(&out.stderr);
            if !err.trim().is_empty() {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(&err);
            }
            RunOutput {
                code: out.status.code(),
                output: tail(&strip_ansi(&text)),
                timed_out: false,
                secs: started.elapsed().as_secs_f32(),
            }
        }
        Ok(Err(e)) => RunOutput { code: None, output: e.to_string(), timed_out: false, secs: started.elapsed().as_secs_f32() },
        Err(_) => RunOutput {
            code: None,
            output: format!("(timeout after {}s)", timeout.as_secs()),
            timed_out: true,
            secs: started.elapsed().as_secs_f32(),
        },
    }
}

/// Retire les séquences d'échappement ANSI (couleurs, déplacements de curseur).
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for n in chars.by_ref() {
                    if ('@'..='~').contains(&n) {
                        break;
                    }
                }
            }
            continue;
        }
        if c == '\r' {
            continue;
        }
        out.push(c);
    }
    out
}

/// Garde la fin de la sortie : c'est là que se trouvent les erreurs.
fn tail(text: &str) -> String {
    let lines: Vec<&str> = text.trim_end().lines().collect();
    let skipped = lines.len().saturating_sub(MAX_OUTPUT_LINES);
    let mut out = lines[skipped..].join("\n");
    if out.chars().count() > MAX_OUTPUT_CHARS {
        let start = out.chars().count() - MAX_OUTPUT_CHARS;
        out = out.chars().skip(start).collect();
        out.insert(0, '…');
    }
    if skipped > 0 {
        out.insert_str(0, &format!("… ({skipped} earlier lines omitted)\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_colour_codes() {
        assert_eq!(strip_ansi("\u{1b}[1;31merror\u{1b}[0m: boom\r\n"), "error: boom\n");
    }

    #[test]
    fn keeps_only_the_end_of_long_output() {
        let long = (0..300).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let t = tail(&long);
        assert!(t.starts_with("… (180 earlier lines omitted)"));
        assert!(t.ends_with("line 299"));
    }

    #[tokio::test]
    async fn runs_a_command_and_captures_exit_code() {
        let out = run("echo bonjour", Path::new("."), Duration::from_secs(20)).await;
        assert!(out.ok());
        assert!(out.output.contains("bonjour"));
        let out = run("exit 3", Path::new("."), Duration::from_secs(20)).await;
        assert_eq!(out.code, Some(3));
    }
}
