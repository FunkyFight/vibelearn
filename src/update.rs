//! Self-update from the project's GitHub releases.
//!
//! `vibelearn update` installs the latest release now. During a session the
//! same check runs in the background (unless `auto-update` is off): a newer
//! release is downloaded and put in place of this executable, and takes
//! effect at the next launch.
//!
//! The repository is the `repository` field of Cargo.toml. A release must
//! carry the raw executable as an asset, not an archive (see [`pick_asset`]).

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::i18n::tr;
use crate::paths;

const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
const CURRENT: &str = env!("CARGO_PKG_VERSION");
const GITHUB_API: &str = "https://api.github.com";

/// Where to look for releases: the API to ask, and `owner/name`.
struct Source {
    api: String,
    slug: String,
}

impl Source {
    /// The project's own repository on GitHub, or nothing if Cargo.toml names none.
    fn github() -> Option<Self> {
        let rest = REPOSITORY.trim_end_matches('/').trim_end_matches(".git").strip_prefix("https://github.com/")?;
        let valid = rest.split('/').count() == 2 && rest.split('/').all(|part| !part.is_empty());
        valid.then(|| Self { api: GITHUB_API.to_string(), slug: rest.to_string() })
    }
}

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Debug, Clone, Deserialize)]
struct Asset {
    name: String,
    /// API address of the asset: it serves the file itself when asked for `application/octet-stream`.
    url: String,
    size: u64,
}

/// What the updater remembers, in `~/.vibelearn/update.json`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    /// Tag of the release it installed: the same tag is never downloaded twice,
    /// even if the executable inside reports an older version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    installed: Option<String>,
    /// Set when a release was just installed, until the next session has told the developer.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    announce: bool,
}

impl State {
    fn load(file: &Path) -> Self {
        std::fs::read_to_string(file).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
    }

    fn save(&self, file: &Path) -> Result<()> {
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(file, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// No release is published yet.
    NoRelease,
    /// The latest release (its tag) is not newer than this executable.
    UpToDate(String),
    /// The latest release has no executable for this machine: its tag, and the files it carries.
    NoAsset(String, Vec<String>),
    /// The release with this tag is now in place.
    Installed(String),
}

/// `v1.2.3`, `1.2` or `1.2.3-beta` as three numbers. Nothing if the tag is not a version.
fn version_numbers(tag: &str) -> Option<[u64; 3]> {
    let core = tag.trim().trim_start_matches(['v', 'V']);
    let core = core.split(['-', '+']).next().unwrap_or(core);
    let mut numbers = [0u64; 3];
    for (slot, part) in numbers.iter_mut().zip(core.split('.')) {
        *slot = part.parse().ok()?;
    }
    (!core.is_empty()).then_some(numbers)
}

/// True when `tag` is a version strictly above `current`. A tag that is not a version never is.
fn is_newer(tag: &str, current: &str) -> bool {
    match (version_numbers(tag), version_numbers(current)) {
        (Some(latest), Some(current)) => latest > current,
        _ => false,
    }
}

fn system_of(name: &str) -> Option<&'static str> {
    // "darwin" contains "win": the Mac names are checked first.
    if ["darwin", "macos", "apple", "osx"].iter().any(|word| name.contains(word)) {
        Some("macos")
    } else if name.contains("linux") {
        Some("linux")
    } else if name.contains("win") || name.ends_with(".exe") {
        Some("windows")
    } else {
        None
    }
}

fn arch_of(name: &str) -> Option<&'static str> {
    if ["aarch64", "arm64"].iter().any(|word| name.contains(word)) {
        Some("aarch64")
    } else if ["x86_64", "amd64", "x64"].iter().any(|word| name.contains(word)) {
        Some("x86_64")
    } else {
        None
    }
}

/// Files a release may carry next to the executables.
const NOT_EXECUTABLE: &[&str] =
    &[".zip", ".gz", ".tgz", ".xz", ".bz2", ".zst", ".7z", ".txt", ".md", ".json", ".sha256", ".sig", ".asc", ".vsix", ".deb", ".rpm", ".dmg", ".msi", ".pdb"];

/// The asset to install on this machine. It is the raw executable:
/// - on Windows, a `.exe` (`vibelearn.exe`, `vibelearn-windows-x86_64.exe`…);
/// - elsewhere, a file whose name says the system (`vibelearn-linux-x86_64`, `vibelearn-macos-arm64`…).
///
/// A name that says another architecture is skipped. Among the rest, the one
/// that says this architecture wins over one that says none.
fn pick_asset<'a>(assets: &'a [Asset], os: &str, arch: &str) -> Option<&'a Asset> {
    let mut best: Option<(bool, &Asset)> = None;
    for asset in assets {
        let name = asset.name.to_lowercase();
        if NOT_EXECUTABLE.iter().any(|end| name.ends_with(end)) || system_of(&name) != Some(os) {
            continue;
        }
        if os == "windows" && !name.ends_with(".exe") {
            continue;
        }
        let exact = match arch_of(&name) {
            Some(named) if named != arch => continue,
            named => named.is_some(),
        };
        if best.is_none_or(|(best_exact, _)| exact && !best_exact) {
            best = Some((exact, asset));
        }
    }
    best.map(|(_, asset)| asset)
}

/// The first bytes of an executable for this system: an error page saved as a file never passes.
fn looks_executable(bytes: &[u8], os: &str) -> bool {
    match os {
        "windows" => bytes.starts_with(b"MZ"),
        "linux" => bytes.starts_with(b"\x7fELF"),
        "macos" => matches!(bytes.get(..4), Some([0xCF | 0xCE, 0xFA, 0xED, 0xFE] | [0xCA, 0xFE, 0xBA, 0xBE])),
        _ => true,
    }
}

fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        // GitHub refuses requests without a User-Agent.
        .user_agent(concat!("vibelearn/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(8))
        .build()?)
}

/// The latest published release (drafts and pre-releases do not count), or nothing if there is none.
async fn latest(client: &reqwest::Client, source: &Source) -> Result<Option<Release>> {
    let url = format!("{}/repos/{}/releases/latest", source.api, source.slug);
    let response = client
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .with_context(|| tr("GitHub est injoignable", "GitHub cannot be reached"))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !response.status().is_success() {
        bail!("{} ({})", tr("GitHub a refusé la demande", "GitHub refused the request"), response.status());
    }
    Ok(Some(response.json().await.context("release")?))
}

async fn download(client: &reqwest::Client, asset: &Asset) -> Result<Vec<u8>> {
    let response = client
        .get(&asset.url)
        .header("Accept", "application/octet-stream")
        .timeout(Duration::from_secs(600))
        .send()
        .await
        .with_context(|| format!("{} {}", tr("téléchargement de", "downloading"), asset.name))?;
    if !response.status().is_success() {
        bail!("{} {} ({})", tr("téléchargement refusé pour", "download refused for"), asset.name, response.status());
    }
    let bytes = response.bytes().await.with_context(|| format!("{} {}", tr("téléchargement de", "downloading"), asset.name))?;
    if bytes.len() as u64 != asset.size {
        bail!(
            "{} {} : {} / {} {}",
            tr("téléchargement incomplet de", "incomplete download of"),
            asset.name,
            bytes.len(),
            asset.size,
            tr("octets", "bytes"),
        );
    }
    Ok(bytes.to_vec())
}

/// `vibelearn.exe` + `.old` = `vibelearn.exe.old`, next to it.
fn sibling(exe: &Path, suffix: &str) -> PathBuf {
    let mut name = exe.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    exe.with_file_name(name)
}

/// Puts `new` in place of `exe`. A running executable cannot be overwritten on
/// Windows, but it can be renamed: it is moved aside, and removed at a later
/// launch once nothing runs it any more (see [`cleanup`]).
fn swap(exe: &Path, new: &Path) -> std::io::Result<()> {
    // A previous one may still be running under the first name: take the first free one.
    let aside = (0..16)
        .map(|n| sibling(exe, &if n == 0 { ".old".to_string() } else { format!(".old{n}") }))
        .find(|path| {
            let _ = std::fs::remove_file(path);
            !path.exists()
        })
        .ok_or_else(|| std::io::Error::other("too many old executables next to this one"))?;
    std::fs::rename(exe, &aside)?;
    if let Err(e) = std::fs::rename(new, exe) {
        // Nothing in place of the executable: put it back.
        let _ = std::fs::rename(&aside, exe);
        return Err(e);
    }
    Ok(())
}

#[cfg(unix)]
fn make_executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Checks the latest release and installs it over `exe` if it is newer than
/// `current` (or whatever it is, with `force`).
async fn install(source: &Source, exe: &Path, state_file: &Path, current: &str, force: bool) -> Result<Outcome> {
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    let client = client()?;
    let Some(release) = latest(&client, source).await? else { return Ok(Outcome::NoRelease) };
    let tag = release.tag_name.clone();
    let mut state = State::load(state_file);
    if !force && (state.installed.as_deref() == Some(tag.as_str()) || !is_newer(&tag, current)) {
        return Ok(Outcome::UpToDate(tag));
    }
    let Some(asset) = pick_asset(&release.assets, os, arch) else {
        return Ok(Outcome::NoAsset(tag, release.assets.iter().map(|a| a.name.clone()).collect()));
    };
    let bytes = download(&client, asset).await?;
    if !looks_executable(&bytes, os) {
        bail!("{} {}", asset.name, tr("n'est pas un exécutable pour ce système", "is not an executable for this system"));
    }
    let staged = sibling(exe, &format!(".new-{}", std::process::id()));
    let placed = std::fs::write(&staged, &bytes).and_then(|_| make_executable(&staged)).and_then(|_| swap(exe, &staged));
    if let Err(e) = placed {
        let _ = std::fs::remove_file(&staged);
        return Err(e).with_context(|| format!("{} {}", tr("remplacement de", "replacing"), exe.display()));
    }
    state.installed = Some(tag.clone());
    state.announce = true;
    // The executable is in place: a state that cannot be written only costs a second download.
    let _ = state.save(state_file);
    Ok(Outcome::Installed(tag))
}

fn state_file() -> Result<PathBuf> {
    Ok(paths::global_dir()?.join("update.json"))
}

async fn install_here(force: bool) -> Result<Outcome> {
    let source = Source::github().with_context(|| {
        tr("ce binaire ne connaît pas son dépôt GitHub (`repository` dans Cargo.toml)", "this binary does not know its GitHub repository (`repository` in Cargo.toml)")
    })?;
    let exe = std::env::current_exe().context("current_exe")?;
    install(&source, &exe, &state_file()?, CURRENT, force).await
}

/// `vibelearn update` : installs the latest release now, and says what it did.
pub async fn run(force: bool) -> Result<()> {
    cleanup();
    println!("vibelearn {CURRENT}");
    match install_here(force).await? {
        Outcome::NoRelease => println!("  {}", tr("Aucune release publiée pour l'instant.", "No release published yet.")),
        Outcome::UpToDate(tag) => println!("  {} {tag}).", tr("Déjà à jour (dernière release :", "Already up to date (latest release:")),
        Outcome::NoAsset(tag, names) => bail!(
            "{} {tag} {} ({} {}). {} {}",
            tr("la release", "release"),
            tr("n'a pas d'exécutable pour ce système", "has no executable for this system"),
            std::env::consts::OS,
            std::env::consts::ARCH,
            tr("Fichiers :", "Files:"),
            if names.is_empty() { "-".to_string() } else { names.join(", ") },
        ),
        Outcome::Installed(tag) => {
            // Said here: the next session has nothing left to announce.
            if let Ok(file) = state_file() {
                let mut state = State::load(&file);
                state.announce = false;
                let _ = state.save(&file);
            }
            println!("  {} {tag}.", tr("Mis à jour vers la release", "Updated to release"));
        }
    }
    Ok(())
}

/// During a session: looks for a newer release in the background and installs
/// it for the next launch. Silent unless it installed something.
pub fn spawn_auto() {
    // A build under development is not a release to replace.
    if cfg!(debug_assertions) {
        return;
    }
    tokio::spawn(async {
        if let Ok(Outcome::Installed(tag)) = install_here(false).await {
            eprintln!(
                "  {} {tag} {}",
                tr("vibelearn : la release", "vibelearn: release"),
                tr("est installée, elle servira au prochain lancement.", "is installed and will be used at the next launch."),
            );
        }
    });
}

/// At the start of a session: the sentence to show if the previous launch
/// installed a release, once.
pub fn announce() -> Option<String> {
    let file = state_file().ok()?;
    let mut state = State::load(&file);
    if !state.announce {
        return None;
    }
    state.announce = false;
    let _ = state.save(&file);
    let tag = state.installed?;
    Some(format!("{} {tag}.", tr("vibelearn a été mis à jour : tu utilises la release", "vibelearn was updated: you are running release")))
}

/// Removes what earlier updates left next to this executable: the ones moved
/// aside (once nothing runs them) and downloads that never finished.
pub fn cleanup() {
    let Ok(exe) = std::env::current_exe() else { return };
    let (Some(dir), Some(name)) = (exe.parent(), exe.file_name().and_then(|n| n.to_str())) else { return };
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let (old, new) = (format!("{name}.old"), format!("{name}.new-"));
    for entry in entries.flatten() {
        let file = entry.file_name();
        let Some(file) = file.to_str() else { continue };
        if file.starts_with(&old) || file.starts_with(&new) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    fn asset(name: &str) -> Asset {
        Asset { name: name.to_string(), url: String::new(), size: 0 }
    }

    #[test]
    fn a_tag_is_newer_only_when_it_is_a_higher_version() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("0.1.1", "0.1.0"));
        assert!(is_newer("v1", "0.9.9"));
        assert!(is_newer("v0.10.0", "0.9.0"), "numbers, not text");
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("v0.0.9", "0.1.0"));
        assert!(!is_newer("nightly", "0.1.0"), "a tag that is not a version never replaces anything");
        assert_eq!(version_numbers("v1.2.3-beta+7"), Some([1, 2, 3]));
        assert_eq!(version_numbers(""), None);
    }

    #[test]
    fn the_asset_is_the_executable_for_this_system_and_architecture() {
        let assets: Vec<Asset> = [
            "vibelearn-0.2.0.zip",
            "vibelearn-linux-x86_64",
            "vibelearn-linux-aarch64",
            "vibelearn-macos-arm64",
            "vibelearn-darwin-x64",
            "vibelearn-windows-arm64.exe",
            "vibelearn-windows-x86_64.exe",
            "vibelearn.exe.sha256",
        ]
        .iter()
        .map(|n| asset(n))
        .collect();
        let pick = |os, arch| pick_asset(&assets, os, arch).map(|a| a.name.as_str());
        assert_eq!(pick("windows", "x86_64"), Some("vibelearn-windows-x86_64.exe"));
        assert_eq!(pick("windows", "aarch64"), Some("vibelearn-windows-arm64.exe"));
        assert_eq!(pick("linux", "aarch64"), Some("vibelearn-linux-aarch64"));
        assert_eq!(pick("macos", "x86_64"), Some("vibelearn-darwin-x64"), "\"darwin\" is not Windows");
        assert_eq!(pick("macos", "aarch64"), Some("vibelearn-macos-arm64"));

        let plain = [asset("vibelearn.exe")];
        assert_eq!(pick_asset(&plain, "windows", "x86_64").map(|a| a.name.as_str()), Some("vibelearn.exe"));
        assert_eq!(pick_asset(&plain, "linux", "x86_64").map(|a| a.name.as_str()), None);
        let both = [asset("vibelearn.exe"), asset("vibelearn-x64.exe")];
        assert_eq!(pick_asset(&both, "windows", "x86_64").map(|a| a.name.as_str()), Some("vibelearn-x64.exe"));
        assert_eq!(pick_asset(&both, "windows", "aarch64").map(|a| a.name.as_str()), Some("vibelearn.exe"));
    }

    #[test]
    fn the_repository_comes_from_cargo_toml() {
        let source = Source::github().expect("`repository` in Cargo.toml must be a GitHub address");
        assert_eq!(source.slug.split('/').count(), 2);
    }

    /// A stand-in for GitHub: one release whose asset is served by the same address.
    fn serve(tag: &'static str, asset_name: String, body: Vec<u8>) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let release = serde_json::json!({
            "tag_name": tag,
            "assets": [{ "name": asset_name, "url": format!("{base}/asset"), "size": body.len() }],
        })
        .to_string()
        .into_bytes();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut request = [0u8; 2048];
                let n = stream.read(&mut request).unwrap_or(0);
                let request = String::from_utf8_lossy(&request[..n]).to_string();
                let path = request.split_whitespace().nth(1).unwrap_or("");
                let (status, content) = match path {
                    "/repos/someone/tool/releases/latest" => ("200 OK", release.clone()),
                    "/asset" => ("200 OK", body.clone()),
                    _ => ("404 Not Found", b"{}".to_vec()),
                };
                let head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", content.len());
                let _ = stream.write_all(head.as_bytes()).and_then(|_| stream.write_all(&content));
            }
        });
        base
    }

    fn fake_executable() -> (String, Vec<u8>) {
        let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
        let name = if os == "windows" { "tool.exe".to_string() } else { format!("tool-{os}-{arch}") };
        let magic: &[u8] = match os {
            "windows" => b"MZ",
            "linux" => b"\x7fELF",
            _ => &[0xCF, 0xFA, 0xED, 0xFE],
        };
        (name, [magic, b" the new version"].concat())
    }

    #[tokio::test]
    async fn a_newer_release_replaces_the_executable_once() {
        let dir = std::env::temp_dir().join(format!("vibelearn-update-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("tool.exe");
        let state = dir.join("update.json");
        std::fs::write(&exe, b"the old version").unwrap();

        let (name, body) = fake_executable();
        let source = Source { api: serve("v0.2.0", name, body.clone()), slug: "someone/tool".to_string() };

        // Not newer than what runs: nothing is touched.
        assert_eq!(install(&source, &exe, &state, "0.2.0", false).await.unwrap(), Outcome::UpToDate("v0.2.0".into()));
        assert_eq!(std::fs::read(&exe).unwrap(), b"the old version");

        // Newer: the asset takes the place of the executable, the old one is kept aside.
        assert_eq!(install(&source, &exe, &state, "0.1.0", false).await.unwrap(), Outcome::Installed("v0.2.0".into()));
        assert_eq!(std::fs::read(&exe).unwrap(), body);
        assert_eq!(std::fs::read(sibling(&exe, ".old")).unwrap(), b"the old version");
        assert!(State::load(&state).announce);

        // The executable inside still says 0.1.0: the tag is remembered, not downloaded again.
        assert_eq!(install(&source, &exe, &state, "0.1.0", false).await.unwrap(), Outcome::UpToDate("v0.2.0".into()));
        // Unless asked to.
        assert_eq!(install(&source, &exe, &state, "0.1.0", true).await.unwrap(), Outcome::Installed("v0.2.0".into()));
        assert_eq!(std::fs::read(&exe).unwrap(), body);

        // No release at all.
        let empty = Source { api: source.api.clone(), slug: "someone/nothing".to_string() };
        assert_eq!(install(&empty, &exe, &state, "0.1.0", false).await.unwrap(), Outcome::NoRelease);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Not a test of its own: the process that `a_running_executable_can_be_replaced` keeps alive.
    #[test]
    #[ignore = "started by a_running_executable_can_be_replaced"]
    fn stay_alive_for_a_while() {
        std::thread::sleep(Duration::from_secs(20));
    }

    /// The case that matters on Windows: the updater replaces the very file that is running.
    #[test]
    fn a_running_executable_can_be_replaced() {
        let dir = std::env::temp_dir().join(format!("vibelearn-update-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join(if cfg!(windows) { "tool.exe" } else { "tool" });
        std::fs::copy(std::env::current_exe().unwrap(), &exe).unwrap();
        let mut running = std::process::Command::new(&exe)
            .args(["--exact", "update::tests::stay_alive_for_a_while", "--ignored"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(800));
        assert!(running.try_wait().unwrap().is_none(), "the copy must still be running");

        let new = sibling(&exe, ".new-1");
        std::fs::write(&new, b"the new version").unwrap();
        swap(&exe, &new).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"the new version");
        assert!(sibling(&exe, ".old").exists(), "the running one is kept aside");

        // A second update while the first old one still runs takes the next free name.
        let newer = sibling(&exe, ".new-2");
        std::fs::write(&newer, b"a newer version").unwrap();
        swap(&exe, &newer).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"a newer version");

        let _ = running.kill();
        let _ = running.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_file_that_is_not_an_executable_is_refused() {
        let dir = std::env::temp_dir().join(format!("vibelearn-update-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("tool.exe");
        std::fs::write(&exe, b"the old version").unwrap();
        let (name, _) = fake_executable();
        let source = Source { api: serve("v9.0.0", name, b"<html>not found</html>".to_vec()), slug: "someone/tool".to_string() };
        assert!(install(&source, &exe, &dir.join("update.json"), "0.1.0", false).await.is_err());
        assert_eq!(std::fs::read(&exe).unwrap(), b"the old version");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "nothing is left next to the executable");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
