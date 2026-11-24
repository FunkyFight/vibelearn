//! VS Code : la commande `code` (ouvrir un dossier ou un fichier) et
//! l'extension de vibelearn, embarquée dans le binaire. vibelearn fabrique le
//! paquet `.vsix` et l'installe avec `code --install-extension` quand la
//! version installée n'est pas celle-ci.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::i18n::tr;

const EXTENSION_JS: &str = include_str!("../vscode/extension.js");
const PACKAGE_JSON: &str = include_str!("../vscode/package.json");
/// Command titles and description: English by default, French for a French editor.
const PACKAGE_NLS: &str = include_str!("../vscode/package.nls.json");
const PACKAGE_NLS_FR: &str = include_str!("../vscode/package.nls.fr.json");
/// `éditeur.nom` de l'extension.
pub const ID: &str = "vibelearn.vibelearn";

// ---------------------------------------------------------------------------
// La commande `code`
// ---------------------------------------------------------------------------

/// Commande qui pilote l'éditeur de l'apprenant (`code` par défaut).
#[derive(Clone)]
pub struct Editor {
    command: Option<String>,
    /// L'éditeur comprend `-g fichier:ligne` (VS Code et ses dérivés).
    goto: bool,
}

/// Vrai une fois que l'échec de l'éditeur a été signalé (on ne le dit qu'une fois).
static EDITOR_WARNED: AtomicBool = AtomicBool::new(false);

impl Editor {
    pub fn new(command: Option<&str>) -> Self {
        let command = match command.map(str::trim) {
            Some("none") | Some("") => None,
            Some(c) => Some(c.to_string()),
            None => Some("code".to_string()),
        };
        let goto = command.as_deref().is_some_and(understands_goto);
        Self { command, goto }
    }

    pub fn enabled(&self) -> bool {
        self.command.is_some()
    }

    /// Arguments pour ouvrir `path` (à la ligne donnée, à partir de 1).
    fn open_arguments(&self, path: &Path, line: Option<usize>) -> Vec<String> {
        match line {
            Some(l) if self.goto => vec!["-g".to_string(), format!("{}:{l}", path.display())],
            _ => vec![path.display().to_string()],
        }
    }

    /// Ouvre un fichier ou un dossier sans attendre l'éditeur.
    pub fn open(&self, path: &Path, line: Option<usize>) {
        let Some(command) = &self.command else { return };
        let mut cmd = editor_process(command, &self.open_arguments(path, line));
        cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        let command = command.clone();
        match cmd.spawn() {
            // `code` rend la main tout de suite ; un éditeur bloquant reste simplement attendu en fond.
            Ok(mut child) => {
                std::thread::spawn(move || {
                    if !child.wait().is_ok_and(|status| status.success()) {
                        warn_editor(&command);
                    }
                });
            }
            Err(_) => warn_editor(&command),
        }
    }

    /// Lance l'éditeur et attend sa sortie (stdout, ou l'erreur).
    fn run(&self, args: &[String]) -> Result<String, String> {
        let command = self.command.as_deref().ok_or("no editor")?;
        let out = editor_process(command, args)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("{command} : {e}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            let mut msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
            if msg.is_empty() {
                msg = String::from_utf8_lossy(&out.stdout).trim().to_string();
            }
            Err(msg)
        }
    }
}

fn warn_editor(command: &str) {
    if !EDITOR_WARNED.swap(true, Ordering::Relaxed) {
        eprintln!(
            "  {} (`{command}`). {}",
            tr("Impossible de lancer VS Code", "Could not start VS Code"),
            tr(
                "Ouvre ce dossier dans VS Code toi-même, ou indique la commande avec --editor <commande>.",
                "Open this folder in VS Code yourself, or name the command with --editor <command>.",
            ),
        );
    }
}

/// VS Code et ses dérivés acceptent `-g fichier:ligne`.
fn understands_goto(command: &str) -> bool {
    // Un chemin complet (qui peut contenir des espaces), ou une commande suivie d'options.
    let whole = command.trim().trim_matches('"');
    let first = whole.split_whitespace().next().unwrap_or(whole);
    [whole, first].iter().any(|program| {
        let stem = Path::new(program).file_stem().and_then(|s| s.to_str()).unwrap_or(program).to_lowercase();
        stem.starts_with("code") || ["codium", "vscodium", "cursor", "windsurf"].contains(&stem.as_str())
    })
}

#[cfg(windows)]
fn editor_process(command: &str, args: &[String]) -> std::process::Command {
    use std::os::windows::process::CommandExt;
    // `code` est un script .cmd : il faut passer par cmd. Avec /S et une paire de
    // guillemets autour de toute la ligne, cmd ne retire que cette paire-là : les
    // chemins entre guillemets (espaces, accents) arrivent intacts.
    let mut line = if Path::new(command).is_file() { format!("\"{command}\"") } else { command.to_string() };
    for arg in args {
        if arg.starts_with('-') {
            line.push_str(&format!(" {arg}"));
        } else {
            line.push_str(&format!(" \"{arg}\""));
        }
    }
    let mut cmd = std::process::Command::new("cmd");
    cmd.args(["/D", "/S", "/C"]).raw_arg(format!("\"{line}\""));
    cmd
}

#[cfg(not(windows))]
fn editor_process(command: &str, args: &[String]) -> std::process::Command {
    let mut parts = command.split_whitespace();
    let mut cmd = std::process::Command::new(parts.next().unwrap_or(command));
    cmd.args(parts).args(args);
    cmd
}

// ---------------------------------------------------------------------------
// L'extension
// ---------------------------------------------------------------------------

/// Version de l'extension : elle change dès que son code change.
pub fn version() -> String {
    let mut bytes = EXTENSION_JS.as_bytes().to_vec();
    bytes.extend_from_slice(PACKAGE_JSON.as_bytes());
    bytes.extend_from_slice(PACKAGE_NLS.as_bytes());
    bytes.extend_from_slice(PACKAGE_NLS_FR.as_bytes());
    format!("0.1.{}", crc32(&bytes) % 1_000_000)
}

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension=".json" ContentType="application/json"/>
  <Default Extension=".js" ContentType="application/javascript"/>
  <Default Extension=".vsixmanifest" ContentType="text/xml"/>
</Types>
"#;

fn manifest(version: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<PackageManifest Version="2.0.0" xmlns="http://schemas.microsoft.com/developer/vsx-schema/2011" xmlns:d="http://schemas.microsoft.com/developer/vsx-schema-design/2011">
  <Metadata>
    <Identity Language="en-US" Id="vibelearn" Version="{version}" Publisher="vibelearn"/>
    <DisplayName>vibelearn</DisplayName>
    <Description xml:space="preserve">vibelearn</Description>
    <Categories>Education</Categories>
    <Properties>
      <Property Id="Microsoft.VisualStudio.Code.Engine" Value="^1.75.0"/>
    </Properties>
  </Metadata>
  <Installation>
    <InstallationTarget Id="Microsoft.VisualStudio.Code"/>
  </Installation>
  <Dependencies/>
  <Assets>
    <Asset Type="Microsoft.VisualStudio.Code.Manifest" Path="extension/package.json" Addressable="true"/>
  </Assets>
</PackageManifest>
"#
    )
}

/// Le paquet `.vsix` (une archive zip).
pub fn vsix() -> Vec<u8> {
    let v = version();
    let files: Vec<(&str, Vec<u8>)> = vec![
        ("[Content_Types].xml", CONTENT_TYPES.as_bytes().to_vec()),
        ("extension.vsixmanifest", manifest(&v).into_bytes()),
        ("extension/package.json", PACKAGE_JSON.replace("__VERSION__", &v).into_bytes()),
        ("extension/package.nls.json", PACKAGE_NLS.as_bytes().to_vec()),
        ("extension/package.nls.fr.json", PACKAGE_NLS_FR.as_bytes().to_vec()),
        ("extension/extension.js", EXTENSION_JS.replace("__VERSION__", &v).into_bytes()),
    ];
    zip(&files)
}

pub enum Install {
    AlreadyThere,
    Installed,
    Failed(String),
}

/// Installe l'extension si la bonne version n'est pas déjà là. Le paquet est
/// écrit dans `dir` le temps de l'installation.
pub fn ensure_installed(editor: &Editor, dir: &Path) -> Install {
    let wanted = format!("{ID}@{}", version());
    match editor.run(&["--list-extensions".into(), "--show-versions".into()]) {
        Ok(list) if list.lines().any(|l| l.trim().eq_ignore_ascii_case(&wanted)) => return Install::AlreadyThere,
        Ok(_) => {}
        Err(e) => return Install::Failed(e),
    }
    let path = dir.join(format!("vibelearn-{}.vsix", version()));
    if let Err(e) = std::fs::create_dir_all(dir).and_then(|_| std::fs::write(&path, vsix())) {
        return Install::Failed(e.to_string());
    }
    let result = editor.run(&["--install-extension".into(), path.display().to_string(), "--force".into()]);
    let _ = std::fs::remove_file(&path);
    match result {
        Ok(_) => Install::Installed,
        Err(e) => Install::Failed(e),
    }
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// Archive zip sans compression.
fn zip(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
    // 1er janvier 1980, minuit : la date n'a pas d'importance ici.
    const DATE: u16 = 0x21;
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in files {
        let crc = crc32(data);
        let size = data.len() as u32;
        let offset = out.len() as u32;
        let name_len = name.len() as u16;
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        for v in [20u16, 0, 0, 0, DATE] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for v in [crc, size, size] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for v in [name_len, 0] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);

        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        for v in [20u16, 20, 0, 0, 0, DATE] {
            central.extend_from_slice(&v.to_le_bytes());
        }
        for v in [crc, size, size] {
            central.extend_from_slice(&v.to_le_bytes());
        }
        for v in [name_len, 0, 0, 0, 0] {
            central.extend_from_slice(&v.to_le_bytes());
        }
        central.extend_from_slice(&0u32.to_le_bytes());
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let central_offset = out.len() as u32;
    let central_size = central.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    let count = files.len() as u16;
    for v in [0u16, 0, count, count] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&central_size.to_le_bytes());
    out.extend_from_slice(&central_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_the_reference_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn the_package_carries_the_extension_with_its_version() {
        let bytes = vsix();
        assert_eq!(&bytes[..4], &[0x50, 0x4b, 0x03, 0x04]);
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("extension/package.json"));
        assert!(text.contains(&format!("\"version\": \"{}\"", version())));
        assert!(!text.contains("__VERSION__"));
    }

    #[test]
    fn only_editors_that_understand_goto_get_a_line_number() {
        let path = Path::new("src/main.rs");
        let code = Editor::new(None);
        assert_eq!(code.open_arguments(path, Some(12)), vec!["-g", "src/main.rs:12"]);
        assert_eq!(code.open_arguments(path, None), vec!["src/main.rs"]);
        assert!(Editor::new(Some("C:\\Program Files\\Microsoft VS Code\\bin\\code.cmd")).goto);
        assert!(Editor::new(Some("code --user-data-dir D:\\x")).goto);
        assert_eq!(Editor::new(Some("notepad")).open_arguments(path, Some(12)), vec!["src/main.rs"]);
        assert!(!Editor::new(Some("none")).enabled());
    }
}
