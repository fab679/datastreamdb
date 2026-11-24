//! Workspace automation: `cargo xtask <task>`.

use std::collections::BTreeMap;
use std::env;
use std::process::Command;

fn usage() -> &'static str {
    "usage: cargo xtask <codegen|spec|check-versions>"
}

fn main() -> anyhow::Result<()> {
    let task = env::args().nth(1).unwrap_or_default();
    match task.as_str() {
        "codegen" => codegen(),
        "spec" => spec(),
        "check-versions" => check_versions(),
        _ => {
            eprintln!("{usage}", usage = usage());
            std::process::exit(2);
        }
    }
}

/// Regenerate prost bindings, `datastreamdb.h`, and build metadata (M1+).
#[allow(clippy::unnecessary_wraps, reason = "tasks will fail once implemented")]
fn codegen() -> anyhow::Result<()> {
    println!("xtask codegen: nothing generated yet (see tools/xtask/TODO.md)");
    Ok(())
}

/// Dump machine-readable specs: crate versions and (later) the C-ABI symbol list.
#[allow(clippy::unnecessary_wraps, reason = "tasks will fail once implemented")]
fn spec() -> anyhow::Result<()> {
    println!(
        "{{\"workspace_version\": \"{v}\", \"abi\": \"pending\"}}",
        v = env!("CARGO_PKG_VERSION")
    );
    Ok(())
}

/// Workspace root (xtask lives at `<root>/tools/xtask`).
fn workspace_root() -> anyhow::Result<std::path::PathBuf> {
    let dir = env::current_dir()?;
    let mut root = dir.as_path();
    loop {
        if root.join("Cargo.toml").exists() && root.join("crates").is_dir() {
            return Ok(root.to_path_buf());
        }
        root = root
            .parent()
            .ok_or_else(|| anyhow::anyhow!("not inside the datastreamdb workspace"))?;
    }
}

/// Verifies pinned dependencies: the lockfile must be fresh, and every
/// third-party crate should resolve to exactly one version.
///
/// With `--online`, additionally diffs each pin against the newest
/// crates.io release (`cargo update --dry-run -w`); that mode needs
/// network and only reports — it never rewrites `Cargo.toml`.
fn check_versions() -> anyhow::Result<()> {
    let online = env::args().any(|arg| arg == "--online");
    let root = workspace_root()?;

    // 1) Lockfile must match the manifests (`--no-deps` keeps this
    // fully offline; the version census below reads Cargo.lock directly).
    let locked = Command::new("cargo")
        .args([
            "metadata",
            "--locked",
            "--offline",
            "--no-deps",
            "--format-version",
            "1",
        ])
        .current_dir(&root)
        .output()?;
    if !locked.status.success() {
        anyhow::bail!(
            "Cargo.lock is stale; run `cargo update -w`:\n{}",
            String::from_utf8_lossy(&locked.stderr)
        );
    }
    println!("lockfile is fresh");

    // 2) Exactly one resolved version per third-party crate.
    let lock_text =
        std::fs::read_to_string(root.join("Cargo.lock")).map_err(|err| anyhow::anyhow!(err))?;
    let mut versions: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut name: Option<String> = None;
    let mut version: Option<String> = None;
    let mut sourced = false;
    let mut flush =
        |name: &mut Option<String>, version: &mut Option<String>, sourced: &mut bool| {
            if *sourced {
                if let (Some(name), Some(version)) = (name.take(), version.take()) {
                    versions.entry(name).or_default().push(version);
                }
            }
            *name = None;
            *version = None;
            *sourced = false;
        };
    for line in lock_text.lines() {
        let line = line.trim();
        if line == "[[package]]" {
            flush(&mut name, &mut version, &mut sourced);
        } else if let Some(value) = line.strip_prefix("name = ") {
            name = Some(value.trim_matches('"').to_owned());
        } else if let Some(value) = line.strip_prefix("version = ") {
            version = Some(value.trim_matches('"').to_owned());
        } else if line.starts_with("source = ") {
            sourced = true;
        }
    }
    flush(&mut name, &mut version, &mut sourced);
    let mut duplicated = 0;
    for (name, list) in &mut versions {
        list.sort_unstable();
        list.dedup();
        if list.len() > 1 {
            duplicated += 1;
            println!("duplicate: {name} resolves to {}", list.join(", "));
        }
    }
    println!(
        "resolved {} third-party crates ({} duplicated)",
        versions.len(),
        duplicated
    );

    // 3) Optional registry freshness diff.
    if online {
        let dry = Command::new("cargo")
            .args(["update", "--dry-run", "-w"])
            .current_dir(&root)
            .output()?;
        print!("{}", String::from_utf8_lossy(&dry.stdout));
        eprint!("{}", String::from_utf8_lossy(&dry.stderr));
        if !dry.status.success() {
            anyhow::bail!("registry diff failed (network?)");
        }
    } else {
        println!("skipped registry diff (offline); re-run with --online");
    }
    Ok(())
}
