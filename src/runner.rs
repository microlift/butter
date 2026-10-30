//! Runners: Rosemary builds Butter can run bottles with.

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::Paths;
use crate::github::Release;
use crate::process::Reporter;
use crate::settings::{BuildProfile, Settings};
use crate::{github, toolchain, util};

const MANIFEST: &str = "runner.toml";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// Built by Butter from a tagged release's source.
    Source,
    /// The official build from a release's Steam compatibility tool download.
    Binary,
    /// A `rosemary` binary the user pointed Butter at, such as their own build.
    Custom,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Info {
    pub name: String,
    pub kind: Kind,
    /// The release tag a source runner was built from.
    #[serde(default)]
    pub tag: Option<String>,
    /// The commit a development runner was built from.
    #[serde(default)]
    pub commit: Option<String>,
    /// The executable: relative to the runner directory, or absolute.
    pub executable: PathBuf,
    #[serde(default)]
    pub installed: i64,
    #[serde(default)]
    pub rustc: Option<String>,
    #[serde(default)]
    pub profile: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Runner {
    pub id: String,
    pub dir: PathBuf,
    pub info: Info,
}

impl Runner {
    pub fn executable(&self) -> PathBuf {
        self.dir.join(&self.info.executable)
    }

    pub fn is_usable(&self) -> bool {
        self.executable().is_file()
    }

    /// A one-line summary for lists.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        parts.push(match self.info.kind {
            Kind::Source => match &self.info.commit {
                Some(sha) => format!("Built from commit {}", sha.get(..7).unwrap_or(sha)),
                None => "Built from source".to_string(),
            },
            Kind::Binary => "Official build".to_string(),
            Kind::Custom => format!("Custom · {}", self.executable().display()),
            Kind::System => "System install".to_string(), // TODO: compare git hash with latest upstream release
        });
        if let Some(profile) = &self.info.profile {
            parts.push(profile.clone());
        }
        if let Some(rustc) = &self.info.rustc {
            parts.push(format!("Rust {rustc}"));
        }
        if self.info.installed > 0 {
            parts.push(util::format_date(self.info.installed));
        }
        if !self.is_usable() {
            parts.push("executable missing".into());
        }
        parts.join(" · ")
    }

    pub fn command(&self) -> Command {
        Command::new(self.executable())
    }
}

/// The runner id a release tag builds from source as.
pub fn id_for_tag(tag: &str) -> String {
    format!("rosemary-{}", util::slugify(tag))
}

/// The runner id a release's official build installs as.
pub fn id_for_binary(tag: &str) -> String {
    format!("rosemary-{}-official", util::slugify(tag))
}

/// The runner id a commit builds as.
pub fn id_for_commit(sha: &str) -> String {
    format!(
        "rosemary-git-{}",
        util::slugify(sha.get(..12).unwrap_or(sha))
    )
}

/// Installed runners, newest release first, custom runners last.
pub fn installed() -> Vec<Runner> {
    let Ok(entries) = std::fs::read_dir(Paths::get().runners()) else {
        return Vec::new();
    };
    let mut list: Vec<Runner> = entries
        .flatten()
        // Hidden directories are installs still being unpacked.
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .filter_map(|e| load(&e.path()))
        .collect();
    list.sort_by(|a, b| {
        let key = |r: &Runner| {
            (
                r.info.kind == Kind::Custom,
                std::cmp::Reverse(sort_version(r)),
                // Builds of commits have no version; newest first.
                std::cmp::Reverse(if r.info.tag.is_none() {
                    r.info.installed
                } else {
                    0
                }),
            )
        };
        key(a)
            .cmp(&key(b))
            .then_with(|| a.info.name.cmp(&b.info.name))
    });
    list
}

fn sort_version(runner: &Runner) -> Option<semver::Version> {
    let tag = runner.info.tag.as_deref()?;
    semver::Version::parse(tag.trim_start_matches('v')).ok()
}

fn load(dir: &Path) -> Option<Runner> {
    let text = std::fs::read_to_string(dir.join(MANIFEST)).ok()?;
    let info: Info = toml::from_str(&text)
        .map_err(|e| eprintln!("butter: skipping {}: {e}", dir.join(MANIFEST).display()))
        .ok()?;
    Some(Runner {
        id: dir.file_name()?.to_string_lossy().into_owned(),
        dir: dir.to_path_buf(),
        info,
    })
}

pub fn find(id: &str) -> Option<Runner> {
    if id.is_empty() || id.contains('/') {
        return None;
    }
    load(&Paths::get().runners().join(id))
}

fn save(dir: &Path, info: &Info) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)?;
    util::write_atomic(
        &dir.join(MANIFEST),
        toml::to_string_pretty(info)?.as_bytes(),
    )
}

pub fn remove(id: &str) -> anyhow::Result<()> {
    let runner = find(id).context("no such runner")?;
    std::fs::remove_dir_all(&runner.dir)?;
    Ok(())
}

/// Register an existing `rosemary` binary as a runner.
pub fn add_custom(name: &str, executable: &Path) -> anyhow::Result<Runner> {
    if !executable.is_file() {
        bail!("{} is not a file", executable.display());
    }
    let name = name.trim();
    let name = if name.is_empty() {
        "Custom Rosemary"
    } else {
        name
    };
    let mut id = format!("custom-{}", util::slugify(name));
    let base = id.clone();
    let mut n = 2;
    while Paths::get().runners().join(&id).exists() {
        id = format!("{base}-{n}");
        n += 1;
    }
    let dir = Paths::get().runners().join(&id);
    let info = Info {
        name: name.to_string(),
        kind: Kind::Custom,
        tag: None,
        commit: None,
        executable: executable.to_path_buf(),
        installed: util::now(),
        rustc: None,
        profile: None,
    };
    save(&dir, &info)?;
    Ok(Runner { id, dir, info })
}

pub fn add_system(name: &str, executable: &Path) -> anyhow::Result<Runner> {
    if !executable.is_file() {
        bail!("{} is not a file", executable.display());
    }
    let name = name.trim();
    let name = if name.is_empty() {
        "Rosemary (system install)"
    } else {
        name
    };
    let mut id = format!("custom-{}", util::slugify(name));
    let base = id.clone();
    let mut n = 2;
    while Paths::get().runners().join(&id).exists() {
        id = format!("{base}-{n}");
        n += 1;
    }
    let dir = Paths::get().runners().join(&id);
    let (tag, commit) = reported_version(executable);
    let info = Info {
        name: match tag {
            None => name.to_string(),
            Some(ref tag) => format!("Rosemary {} (system install)", tag.trim_start_matches('v')),
        },
        kind: Kind::System,
        tag,
        commit,
        executable: executable.to_path_buf(),
        installed: util::now(),
        rustc: None,
        profile: None,
    };
    save(&dir, &info)?;
    Ok(Runner { id, dir, info })
}

/// The release tag and commit a `rosemary` binary reports, as far as it
/// reports them.
fn reported_version(executable: &Path) -> (Option<String>, Option<String>) {
    match Command::new(executable).arg("--version").output() {
        Ok(output) if output.status.success() => {
            parse_version(&String::from_utf8_lossy(&output.stdout))
        }
        _ => (None, None),
    }
}

/// Parse `rosemary --version`: `rosemary 0.4.1`, then `commit: fd018c65`
/// from builds that know their commit. The version comes back as its release
/// tag, `v0.4.1`.
fn parse_version(text: &str) -> (Option<String>, Option<String>) {
    let mut lines = text.lines();
    let tag = lines
        .next()
        .and_then(|l| l.trim().strip_prefix("rosemary "))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| format!("v{}", v.trim_start_matches('v')));
    // Builds made outside a git checkout print nothing or `unknown`.
    let commit = lines
        .find_map(|l| l.trim().strip_prefix("commit:"))
        .map(str::trim)
        .filter(|c| !c.is_empty() && c.chars().all(|ch| ch.is_ascii_hexdigit()))
        .map(str::to_string);
    (tag, commit)
}

/// Download `url` to `dest`, reporting progress.
pub fn download(url: &str, dest: &Path, reporter: &Reporter) -> anyhow::Result<()> {
    reporter.line(format!("GET {url}"));
    let mut response = github::agent()
        .get(url)
        .call()
        .with_context(|| format!("could not download {url}"))?;
    let total = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let partial = dest.with_extension("part");
    let mut file = std::fs::File::create(&partial)?;
    let mut reader = response.body_mut().with_config().limit(2 << 30).reader();
    let mut buf = vec![0; 64 * 1024];
    let mut done: u64 = 0;
    let mut reported = 0;
    loop {
        reporter.check_cancelled()?;
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        done += n as u64;
        if let Some(total) = total.filter(|t| *t > 0) {
            reporter.progress(done as f64 / total as f64);
        }
        if done - reported >= 4 << 20 {
            reporter.line(format!("  {} downloaded", util::format_size(done)));
            reported = done;
        }
    }
    file.sync_all()?;
    std::fs::rename(&partial, dest)?;
    reporter.line(format!("  {} downloaded", util::format_size(done)));
    Ok(())
}

/// Unpack a GitHub source tarball into `dest`, dropping its top directory.
pub fn unpack_source(archive: &Path, dest: &Path, reporter: &Reporter) -> anyhow::Result<()> {
    if dest.exists() {
        std::fs::remove_dir_all(dest)?;
    }
    std::fs::create_dir_all(dest)?;
    let file = std::fs::File::open(archive)?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    tar.set_preserve_permissions(true);
    for entry in tar.entries()? {
        reporter.check_cancelled()?;
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let mut components = path.components();
        components.next();
        let relative: PathBuf = components.collect();
        if relative.as_os_str().is_empty() {
            continue;
        }
        if relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        {
            bail!("refusing to unpack {}", path.display());
        }
        let target = dest.join(&relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        entry.unpack(&target)?;
    }
    Ok(())
}

/// The SHA-256 of the file at `path`, as lowercase hex.
fn sha256_file(path: &Path, reporter: &Reporter) -> anyhow::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0; 64 * 1024];
    loop {
        reporter.check_cancelled()?;
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// What runs a runner made from the official Steam compatibility tool.
///
/// It does what the tool's own `rosemary-steam` does before handing over:
/// undo the Steam runtime's environment, should Butter itself have been
/// started from Steam, then run the bundle's `bin/rosemary`. That is a
/// launcher which picks the host's or the bundled glibc and puts the bundled
/// libraries (SDL 3, OpenAL and the rest) first. Butter passes the verb and
/// sets `ROSEMARY_PREFIX` itself, so the arguments go through unchanged.
const BINARY_LAUNCHER: &str = r#"#!/bin/sh
# Written by Butter: runs the official Rosemary build in ../Rosemary, the
# Steam compatibility tool from Rosemary's GitHub releases, the way its
# rosemary-steam script does.
set -eu

if [ -n "${STEAM_RUNTIME:-}" ]; then
    LD_LIBRARY_PATH=${SYSTEM_LD_LIBRARY_PATH:-}
    [ -n "${SYSTEM_PATH:-}" ] && PATH=$SYSTEM_PATH
    [ -n "$LD_LIBRARY_PATH" ] || unset LD_LIBRARY_PATH
    unset LD_PRELOAD STEAM_RUNTIME_LIBRARY_PATH
fi

here=${0%/*}
exec "$here/../Rosemary/bin/rosemary" "$@"
"#;

/// Unpack a Steam compatibility tool archive into `dest`, which then holds
/// its `Rosemary/` directory. Entries that would land outside `dest`, also
/// by way of a symbolic link, are refused.
pub fn unpack_tool(archive: &Path, dest: &Path, reporter: &Reporter) -> anyhow::Result<()> {
    std::fs::create_dir_all(dest)?;
    let file = std::fs::File::open(archive)?;
    let reader: Box<dyn Read> = if archive.extension().is_some_and(|e| e == "zst") {
        Box::new(zstd::stream::read::Decoder::new(file)?)
    } else {
        Box::new(flate2::read::GzDecoder::new(file))
    };
    let mut tar = tar::Archive::new(reader);
    tar.set_preserve_permissions(true);
    for entry in tar.entries()? {
        reporter.check_cancelled()?;
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if !entry.unpack_in(dest)? {
            bail!("refusing to unpack {}", path.display());
        }
    }
    if !dest.join("Rosemary/bin/rosemary").is_file() {
        bail!("the download has no Rosemary/bin/rosemary; it is not a Rosemary Steam tool");
    }
    Ok(())
}

/// Download `release`'s official build for this computer, the Steam
/// compatibility tool, and install it as a runner.
pub fn install_binary(release: &Release, reporter: &Reporter) -> anyhow::Result<String> {
    let paths = Paths::get();
    let tag = &release.tag_name;
    let asset = release.steam_tool().with_context(|| {
        format!(
            "Rosemary {tag} has no official build for {}; build it from source instead",
            std::env::consts::ARCH
        )
    })?;

    reporter.stage(format!("Downloading Rosemary {tag}"), Some(0.0));
    let expected = match asset.sha256() {
        Some(hash) => hash,
        None => {
            let sums = release.asset("SHA256SUMS").with_context(|| {
                format!("Rosemary {tag} publishes no checksum for {}", asset.name)
            })?;
            reporter.line(format!("GET {}", sums.browser_download_url));
            let text = github::fetch_text(&sums.browser_download_url)?;
            github::checksum_for(&text, &asset.name)
                .with_context(|| format!("SHA256SUMS does not list {}", asset.name))?
        }
    };
    let archive = paths.downloads().join(&asset.name);
    download(&asset.browser_download_url, &archive, reporter)?;
    let result = install_binary_from(release, &archive, &expected, reporter);
    let _ = std::fs::remove_file(&archive);
    result
}

fn install_binary_from(
    release: &Release,
    archive: &Path,
    expected: &str,
    reporter: &Reporter,
) -> anyhow::Result<String> {
    let tag = &release.tag_name;
    reporter.stage("Verifying the download", Some(0.8));
    let actual = sha256_file(archive, reporter)?;
    reporter.line(format!("sha256 {actual}"));
    if actual != expected {
        bail!("the download is damaged: its SHA-256 is {actual}, but the release lists {expected}");
    }

    reporter.stage("Installing the runner", Some(0.85));
    let id = id_for_binary(tag);
    let runners = Paths::get().runners();
    let dir = runners.join(&id);
    // Unpacked beside the runner and swapped in whole, so a failed or
    // cancelled install leaves any earlier one working.
    let staging = runners.join(format!(".{id}.new"));
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    let staged = (|| {
        unpack_tool(archive, &staging, reporter)?;
        let bin = staging.join("bin");
        std::fs::create_dir_all(&bin)?;
        let launcher = bin.join("rosemary");
        std::fs::write(&launcher, BINARY_LAUNCHER)?;
        std::fs::set_permissions(
            &launcher,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )?;
        let info = Info {
            name: format!("Rosemary {} (Official Build)", tag.trim_start_matches('v')),
            kind: Kind::Binary,
            tag: Some(tag.to_string()),
            commit: None,
            executable: "bin/rosemary".into(),
            installed: util::now(),
            rustc: None,
            profile: None,
        };
        save(&staging, &info)?;
        reporter.check_cancelled()?;
        Ok::<_, anyhow::Error>(info)
    })();
    let info = match staged {
        Ok(info) => info,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e);
        }
    };
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::rename(&staging, &dir)?;
    reporter.progress(1.0);
    Ok(format!("{} is installed", info.name))
}

/// What to build from source: a tagged release or a commit.
#[derive(Debug, Clone)]
pub struct Target {
    pub id: String,
    /// Names it in messages: the tag, or `commit abc1234`.
    pub label: String,
    pub name: String,
    pub slug: String,
    pub url: String,
    pub tag: Option<String>,
    pub commit: Option<String>,
}

impl Target {
    pub fn release(tag: &str) -> Target {
        Target {
            id: id_for_tag(tag),
            label: tag.to_string(),
            name: format!("Rosemary {}", tag.trim_start_matches('v')),
            slug: util::slugify(tag),
            url: github::source_url(tag),
            tag: Some(tag.to_string()),
            commit: None,
        }
    }

    pub fn commit(sha: &str) -> Target {
        let short = sha.get(..7).unwrap_or(sha);
        Target {
            id: id_for_commit(sha),
            label: format!("commit {short}"),
            name: format!("Rosemary dev {short}"),
            slug: format!("git-{}", util::slugify(sha.get(..12).unwrap_or(sha))),
            url: github::commit_source_url(sha),
            tag: None,
            commit: Some(sha.to_string()),
        }
    }
}

/// Download `target`'s source, build it and install it as a runner.
pub fn build_from_source(
    target: &Target,
    settings: &Settings,
    reporter: &Reporter,
) -> anyhow::Result<String> {
    let archive = Paths::get()
        .downloads()
        .join(format!("rosemary-{}.tar.gz", target.slug));
    reporter.stage(format!("Downloading Rosemary {}", target.label), Some(0.0));
    download(&target.url, &archive, reporter)?;
    let result = build_from_archive(target, &archive, settings, reporter);
    let _ = std::fs::remove_file(&archive);
    result
}

/// Build `target` from its source tarball and install it as a runner.
pub fn build_from_archive(
    target: &Target,
    archive: &Path,
    settings: &Settings,
    reporter: &Reporter,
) -> anyhow::Result<String> {
    let paths = Paths::get();
    let id = target.id.clone();
    let tag = &target.label;
    let source = paths.sources().join(&target.slug);

    reporter.stage("Unpacking the source", Some(0.05));
    unpack_source(archive, &source, reporter)?;

    reporter.stage("Checking the build environment", Some(0.08));
    let required = toolchain::required_by_source(&source);
    if let Some(required) = &required {
        reporter.line(format!("Rosemary {tag} needs Rust {required} or newer"));
    }
    let mut chosen = toolchain::best(settings, required.as_ref()).context(
        "no Rust toolchain was found. Install one from the Runners page, \
         or point Butter at cargo in Preferences",
    )?;
    if !chosen.satisfies(required.as_ref()) && toolchain::managed_installed() {
        reporter.line(format!(
            "{} is too old; updating Butter's toolchain",
            chosen.describe()
        ));
        toolchain::install_managed(reporter)?;
        chosen = toolchain::best(settings, required.as_ref()).context("the toolchain vanished")?;
    }
    if !chosen.satisfies(required.as_ref()) {
        bail!(
            "{} is older than the Rust {} this release needs. Install Butter's own \
             toolchain from the Runners page, or update yours",
            chosen.describe(),
            required.map(|r| r.to_string()).unwrap_or_default()
        );
    }
    reporter.line(format!(
        "using {} at {}",
        chosen.describe(),
        chosen.cargo.display()
    ));
    for requirement in toolchain::build_requirements() {
        let mark = match (requirement.found, requirement.optional) {
            (true, _) => "ok",
            (false, true) => "optional",
            (false, false) => "MISSING",
        };
        reporter.line(format!(
            "  {:<11} {mark}: {}",
            requirement.name, requirement.detail
        ));
    }

    reporter.stage("Compiling Rosemary (this takes a while)", Some(0.1));
    let packages = count_packages(&source).max(1);
    let build_dir = paths.build_target();
    let profile = settings.build_profile;
    // Older releases may predate the `fullnative` profile; the flags below
    // still tune for this CPU then.
    let manifest = std::fs::read_to_string(source.join("Cargo.toml")).unwrap_or_default();
    let cargo_profile = match profile {
        BuildProfile::Native if !manifest.contains("[profile.fullnative]") => "release",
        _ => profile.cargo_profile(),
    };
    let mut cargo = chosen.command();
    cargo
        .current_dir(&source)
        .args([
            "build",
            "--locked",
            "--bin",
            "rosemary",
            "--profile",
            cargo_profile,
        ])
        .env("CARGO_TARGET_DIR", &build_dir)
        .env("CARGO_TERM_COLOR", "never")
        .env("CARGO_TERM_PROGRESS_WHEN", "never");
    if profile == BuildProfile::Native {
        cargo
            .env("RUSTFLAGS", "-C target-cpu=native")
            .env("CFLAGS", "-march=native")
            .env("CXXFLAGS", "-march=native");
    }
    let compiled = std::sync::atomic::AtomicUsize::new(0);
    let progress = reporter.clone();
    let status = reporter.run_observed(&mut cargo, move |line| {
        if line.trim_start().starts_with("Compiling ") {
            let n = compiled.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            progress.progress((n as f64 / packages as f64).min(0.99));
        }
    })?;
    if !status.success() {
        bail!("cargo build failed ({status}); see the log for details");
    }

    reporter.stage("Installing the runner", Some(0.95));
    let built = build_dir.join(cargo_profile).join("rosemary");
    let dir = paths.runners().join(&id);
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin)?;
    let staged = bin.join(".rosemary.new");
    std::fs::copy(&built, &staged).with_context(|| format!("copying {}", built.display()))?;
    std::fs::rename(&staged, bin.join("rosemary"))?;
    // Rosemary looks for the guest-facing Steam bridge beside its binary.
    let steamclient = source.join("tools/steamclient/generated");
    for name in ["steamclient.dylib", "steamclient_dispatch.bin"] {
        if steamclient.join(name).is_file() {
            std::fs::copy(steamclient.join(name), bin.join(name))?;
        }
    }
    let info = Info {
        name: target.name.clone(),
        kind: Kind::Source,
        tag: target.tag.clone(),
        commit: target.commit.clone(),
        executable: "bin/rosemary".into(),
        installed: util::now(),
        rustc: Some(chosen.rustc.to_string()),
        profile: Some(profile.label().to_string()),
    };
    save(&dir, &info)?;

    let _ = std::fs::remove_dir_all(&source);
    if !settings.keep_build_cache {
        reporter.line("removing the build cache");
        let _ = std::fs::remove_dir_all(&build_dir);
    }
    reporter.progress(1.0);
    Ok(format!("{} is installed", info.name))
}

/// How many packages `Cargo.lock` lists, to estimate build progress.
fn count_packages(source: &Path) -> usize {
    std::fs::read_to_string(source.join("Cargo.lock"))
        .map(|text| text.lines().filter(|l| l.trim() == "[[package]]").count())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpacks_and_strips_top_directory() {
        let root = std::env::temp_dir().join(format!("butter-unpack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("rosemary-0.2.0/crates")).unwrap();
        std::fs::write(root.join("rosemary-0.2.0/Cargo.toml"), "[workspace]").unwrap();
        std::fs::write(root.join("rosemary-0.2.0/crates/a.rs"), "fn a() {}").unwrap();
        let archive = root.join("src.tar.gz");
        let status = Command::new("tar")
            .args(["-czf"])
            .arg(&archive)
            .arg("-C")
            .arg(&root)
            .arg("rosemary-0.2.0")
            .status()
            .unwrap();
        assert!(status.success());

        let dest = root.join("out");
        let (rx, _) = crate::process::spawn({
            let archive = archive.clone();
            let dest = dest.clone();
            move |r| unpack_source(&archive, &dest, r).map(|_| String::new())
        });
        while let Ok(event) = rx.recv_blocking() {
            if let crate::process::Event::Finished(result) = event {
                result.unwrap();
            }
        }
        assert!(dest.join("Cargo.toml").is_file());
        assert!(dest.join("crates/a.rs").is_file());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ids() {
        assert_eq!(id_for_tag("v0.2.0"), "rosemary-v0.2.0");
        assert_eq!(id_for_binary("v0.2.0"), "rosemary-v0.2.0-official");
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let target = Target::commit(sha);
        assert_eq!(target.id, "rosemary-git-0123456789ab");
        assert_eq!(target.name, "Rosemary dev 0123456");
        assert!(target.tag.is_none());
        assert_eq!(Target::release("v0.2.0").id, "rosemary-v0.2.0");
    }

    #[test]
    fn parses_version() {
        let text = "rosemary 0.4.1\ncommit: fd018c65\nbuilt:  2026-10-30\n";
        assert_eq!(
            parse_version(text),
            (Some("v0.4.1".into()), Some("fd018c65".into()))
        );
        // Built outside git, or by a release that prints only its version.
        assert_eq!(
            parse_version("rosemary 0.4.1\ncommit: unknown\nbuilt:  unknown\n"),
            (Some("v0.4.1".into()), None)
        );
        assert_eq!(
            parse_version("rosemary 0.4.1\ncommit: \n"),
            (Some("v0.4.1".into()), None)
        );
        assert_eq!(
            parse_version("rosemary 0.2.0\n"),
            (Some("v0.2.0".into()), None)
        );
        assert_eq!(parse_version("something else\n"), (None, None));
    }
}

#[cfg(test)]
mod network_tests {
    /// Builds a real release, which needs the network and a full toolchain.
    /// Run with `cargo test -- --ignored build_real_release --nocapture`.
    #[test]
    #[ignore]
    fn build_real_release() {
        let tag = std::env::var("BUTTER_TEST_TAG").unwrap_or_else(|_| "v0.2.0".into());
        let target = super::Target::release(&tag);
        let settings = crate::settings::Settings {
            cargo_override: std::env::var("BUTTER_TEST_CARGO").ok(),
            ..Default::default()
        };
        // A local tarball, as from `git archive`, skips the download.
        let archive = std::env::var_os("BUTTER_TEST_ARCHIVE");
        let (rx, _) = crate::process::spawn(move |r| match archive {
            Some(archive) => super::build_from_archive(&target, archive.as_ref(), &settings, r),
            None => super::build_from_source(&target, &settings, r),
        });
        while let Ok(event) = rx.recv_blocking() {
            match event {
                crate::process::Event::Line(l) => println!("{l}"),
                crate::process::Event::Finished(r) => println!("RESULT: {r:?}"),
                _ => {}
            }
        }
    }

    /// Installs a real release's official build, which needs the network.
    /// Run with `cargo test -- --ignored install_real_binary --nocapture`.
    #[test]
    #[ignore]
    fn install_real_binary() {
        let tag = std::env::var("BUTTER_TEST_TAG").unwrap_or_else(|_| "v0.2.0".into());
        // A local Steam tool archive and its SHA-256 skip the download.
        let archive = std::env::var_os("BUTTER_TEST_TOOL_ARCHIVE");
        let sha256 = std::env::var("BUTTER_TEST_TOOL_SHA256").unwrap_or_default();
        let (rx, _) = crate::process::spawn(move |r| match archive {
            Some(archive) => {
                let release = crate::github::Release {
                    tag_name: tag,
                    name: None,
                    body: None,
                    html_url: String::new(),
                    published_at: None,
                    prerelease: false,
                    draft: false,
                    assets: Vec::new(),
                };
                super::install_binary_from(&release, archive.as_ref(), &sha256, r)
            }
            None => {
                let (releases, _) = crate::github::releases(true)?;
                let release = releases
                    .into_iter()
                    .find(|x| x.tag_name == tag)
                    .ok_or_else(|| anyhow::anyhow!("no release {tag}"))?;
                super::install_binary(&release, r)
            }
        });
        while let Ok(event) = rx.recv_blocking() {
            match event {
                crate::process::Event::Line(l) => println!("{l}"),
                crate::process::Event::Finished(r) => println!("RESULT: {r:?}"),
                _ => {}
            }
        }
    }
}
