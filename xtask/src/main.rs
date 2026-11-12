//! Development tasks of cutwater that stay out of the shipped crate.
//!
//! `cargo run --locked -p xtask -- notices <dir> <target>` writes the
//! third-party notices of the release executable for `<target>` into `<dir>`:
//! `THIRD-PARTY-NOTICES.txt`, with the notices of the crates it is built from
//! (cargo-about, `about.toml` and the template `about.hbs`), of the Rust
//! toolchain's compiler-builtins and, for a Linux (musl) target, of the Zig
//! distribution that links it and of musl libc, and
//! `RUST-LIBRARY-COPYRIGHT.html`, the toolchain's notices of the Rust standard
//! library. The release archives and the npm platform packages carry both.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as Process;

use anyhow::{Context, Result, bail};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [task, dir, target] if task == "notices" => notices(Path::new(dir), target),
        _ => bail!("usage: cargo run -p xtask -- notices <dir> <target>"),
    }
}

fn write(path: &Path, content: &[u8]) -> Result<()> {
    fs::write(path, content).with_context(|| format!("cannot write {}", path.display()))
}

/// The cargo-about release the notices are generated with; CI installs the
/// same one.
const CARGO_ABOUT_VERSION: &str = "0.9.2";

/// The notices file and the copy of the toolchain's notices of the standard
/// library that `notices` writes.
const NOTICES: &str = "THIRD-PARTY-NOTICES.txt";
const RUST_LIBRARY_COPYRIGHT: &str = "RUST-LIBRARY-COPYRIGHT.html";

/// Fails unless `accepted` in about.toml names the licenses that
/// `licenses.allow` in deny.toml names: the notices accept exactly the
/// licenses the dependency check allows.
fn check_accepted(root: &Path) -> Result<()> {
    let list = |file: &str, path: &[&str]| -> Result<BTreeSet<String>> {
        let text =
            fs::read_to_string(root.join(file)).with_context(|| format!("cannot read {file}"))?;
        let mut value: toml::Value =
            toml::from_str(&text).with_context(|| format!("{file} is not TOML"))?;
        for key in path {
            value = value
                .get(key)
                .cloned()
                .with_context(|| format!("{file} has no {}", path.join(".")))?;
        }
        value
            .as_array()
            .with_context(|| format!("{} in {file} is not a list", path.join(".")))?
            .iter()
            .map(|license| {
                license
                    .as_str()
                    .map(str::to_string)
                    .with_context(|| format!("{} in {file} holds a non-string", path.join(".")))
            })
            .collect()
    };
    let accepted = list("about.toml", &["accepted"])?;
    let allowed = list("deny.toml", &["licenses", "allow"])?;
    if accepted != allowed {
        bail!(
            "about.toml accepts {:?}, but deny.toml allows {:?}; they must name the same licenses",
            accepted,
            allowed
        );
    }
    Ok(())
}

/// Writes the third-party notices of the binary for `target` into `dir`:
///
/// - the license texts cargo-about finds for every crate of the target's
///   dependency graph, followed by the NOTICE files of those crates, which
///   the Apache License asks a redistribution to carry and cargo-about does
///   not collect;
/// - the license of compiler-builtins and its libm, which the Rust toolchain
///   links into every binary, from the toolchain's rust-src component, and a
///   copy of the toolchain's COPYRIGHT-library.html, the notices of the
///   standard library and the crates it is built from;
/// - for a Linux (musl) target, the license of the Zig distribution that
///   cargo-zigbuild links the binary with and musl's COPYRIGHT, from that zig
///   installation. Apple's toolchain links a macOS target against the
///   system's libraries, which the binary does not carry, so a macOS target
///   needs neither a Zig notice nor a zig installation.
///
/// Fails when a crate's license cannot be read or is not accepted, on any
/// warning of cargo-about, such as a clarification in `about.toml` whose file
/// no longer matches, and when a source file of a clarified crate carries a
/// copyright line the notices lack.
fn notices(dir: &Path, target: &str) -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("xtask is not inside the workspace")?;
    let dir =
        std::path::absolute(dir).with_context(|| format!("cannot resolve {}", dir.display()))?;
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());

    let version = Process::new(&cargo)
        .args(["about", "--version"])
        .output()
        .context("cannot run cargo-about")?;
    let version = String::from_utf8_lossy(&version.stdout);
    if version.split_whitespace().nth(1) != Some(CARGO_ABOUT_VERSION) {
        bail!(
            "the notices are generated with cargo-about {CARGO_ABOUT_VERSION}, not '{}'; install it \
             with `cargo install --locked --features cli cargo-about@{CARGO_ABOUT_VERSION}`",
            version.trim()
        );
    }

    check_accepted(root)?;

    let about = |args: &[&str]| -> Result<Vec<u8>> {
        let out = Process::new(&cargo)
            .current_dir(root)
            .args(["about", "generate", "--locked", "--fail"])
            .args(["--config", "about.toml", "--target", target])
            .args(args)
            .output()
            .context("cannot run cargo-about")?;
        let stderr = String::from_utf8_lossy(&out.stderr);
        eprint!("{stderr}");
        if !out.status.success() {
            bail!("cargo-about failed with {}", out.status);
        }
        if stderr.contains("WARN") || stderr.contains("ERROR") {
            bail!("cargo-about warned; the notices would be incomplete");
        }
        Ok(out.stdout)
    };

    let mut text =
        String::from_utf8(about(&["about.hbs"])?).context("the notices are not UTF-8")?;
    let graph: serde_json::Value = serde_json::from_slice(&about(&["--format", "json"])?)
        .context("cannot parse cargo-about's JSON output")?;
    let mut crates = Vec::new();
    for krate in graph["crates"]
        .as_array()
        .context("cargo-about listed no crates")?
    {
        let package = &krate["package"];
        let (Some(name), Some(version), Some(manifest)) = (
            package["name"].as_str(),
            package["version"].as_str(),
            package["manifest_path"].as_str(),
        ) else {
            bail!("cargo-about listed a crate without name, version or manifest path");
        };
        let source = Path::new(manifest).parent().unwrap_or(root).to_path_buf();
        for notice in notice_files(&source)? {
            let body = fs::read_to_string(&notice)
                .with_context(|| format!("cannot read {}", notice.display()))?;
            let file_name = notice.file_name().unwrap_or_default().to_string_lossy();
            text.push_str(&format!(
                "\n{}\n{file_name} of {name} {version}\n\n{}\n",
                "-".repeat(80),
                body.trim_end()
            ));
        }
        // The workspace's own crate is the subject of the notices, not a part
        // of them; its license ships beside them.
        if package["source"].is_string() {
            crates.push((name.to_string(), version.to_string(), source));
        }
    }
    let additional = additional_notices(&graph, &crates)?;
    text.push_str(&additional);
    check_copyright_lines(&crates, &text)?;

    let sysroot = rust_sysroot()?;
    text.push_str(&rust_notices(&sysroot)?);
    text.push_str(&zig_notices(target)?);

    fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    write(&dir.join(NOTICES), text.as_bytes())?;
    let library = sysroot.join("share/doc/rust/COPYRIGHT-library.html");
    let library = fs::read(&library).with_context(|| {
        format!(
            "cannot read {}, the Rust toolchain's notices of the standard library",
            library.display()
        )
    })?;
    write(&dir.join(RUST_LIBRARY_COPYRIGHT), &library)
}

/// The copyright lines of a crate's sources that the notices must carry, by
/// line, with the files that hold each, relative to the crate's root.
///
/// Every file of the crate's package counts, except what is not part of the
/// binary or not a notice of the crate's license:
/// - files under a directory named `tests`, `test`, `testdata`, `test-data`,
///   `benches`, `examples`, `fuzz` or starting with `.`: test data,
///   benchmarks, examples and repository metadata;
/// - files at the crate's root whose name starts with `LICENSE`, `LICENCE`,
///   `COPYING`, `UNLICENSE`, `NOTICE` or `COPYRIGHT`: the license texts, whose
///   notices the output carries when the license is the one it carries for the
///   crate, and which belong to an alternative license otherwise; NOTICE files
///   are carried whole.
fn copyright_lines(source: &Path) -> Result<BTreeMap<String, Vec<String>>> {
    const SKIPPED_DIRS: &[&str] = &[
        "tests",
        "test",
        "testdata",
        "test-data",
        "benches",
        "examples",
        "fuzz",
    ];
    const LICENSE_FILES: &[&str] = &[
        "LICENSE",
        "LICENCE",
        "COPYING",
        "UNLICENSE",
        "NOTICE",
        "COPYRIGHT",
    ];
    let mut lines: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for file in source_files(source)? {
        let path = file.strip_prefix(source).unwrap_or(&file);
        let parts: Vec<String> = path
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect();
        let (dirs, name) = parts.split_at(parts.len() - 1);
        if dirs
            .iter()
            .any(|d| d.starts_with('.') || SKIPPED_DIRS.contains(&d.as_str()))
        {
            continue;
        }
        let upper = name[0].to_ascii_uppercase();
        if dirs.is_empty() && LICENSE_FILES.iter().any(|l| upper.starts_with(l)) {
            continue;
        }
        let Ok(content) = fs::read_to_string(&file) else {
            continue;
        };
        let shown = parts.join("/");
        for notice in copyright_notices(&content) {
            let files = lines.entry(notice).or_default();
            if !files.contains(&shown) {
                files.push(shown.clone());
            }
        }
    }
    Ok(lines)
}

/// The copyright notices of a file, each the copyright line with the lines
/// that continue its list of holders, without comment markers and joined by
/// newlines. A line continues the notice while the notice's last line ends
/// with `,`, `;`, `&`, `and` or `\\`, and it is not empty and not a copyright
/// line of its own, as in clap_derive's header, whose copyright line ends
/// with `,` and is followed by two more holders.
fn copyright_notices(content: &str) -> Vec<String> {
    let lines: Vec<&str> = content.lines().collect();
    let mut notices = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if !is_copyright_line(lines[i]) {
            i += 1;
            continue;
        }
        let mut notice = vec![normalized(lines[i])];
        i += 1;
        while i < lines.len() {
            let last = notice.last().map(String::as_str).unwrap_or_default();
            let continues = last.ends_with([',', ';', '&', '\\'])
                || last.ends_with(" and")
                || last.eq_ignore_ascii_case("and");
            let next = normalized(lines[i]);
            if !continues || next.is_empty() || is_copyright_line(lines[i]) {
                break;
            }
            notice.push(next);
            i += 1;
        }
        notices.push(notice.join("\n"));
    }
    notices
}

/// A notice's lines joined as `normalized` text joins the lines of the
/// notices, for a search in them.
fn flattened(notice: &str) -> String {
    notice.split('\n').collect::<Vec<_>>().join(" ")
}

/// The section of the notices that carries, for every crate, the copyright
/// lines of its sources that the license texts cargo-about found for it lack,
/// with the files that hold them; empty when there are none.
fn additional_notices(
    graph: &serde_json::Value,
    crates: &[(String, String, PathBuf)],
) -> Result<String> {
    // The license texts the notices carry for each crate.
    let mut texts: BTreeMap<(String, String), String> = BTreeMap::new();
    for license in graph["licenses"]
        .as_array()
        .context("cargo-about listed no licenses")?
    {
        let text = license["text"].as_str().unwrap_or_default();
        for user in license["used_by"].as_array().into_iter().flatten() {
            let krate = &user["crate"];
            let key = (
                krate["name"].as_str().unwrap_or_default().to_string(),
                krate["version"].as_str().unwrap_or_default().to_string(),
            );
            let entry = texts.entry(key).or_default();
            entry.push_str(&text.lines().map(normalized).collect::<Vec<_>>().join(" "));
            entry.push(' ');
        }
    }
    let mut section = String::new();
    for (name, version, source) in crates {
        let carried = texts
            .get(&(name.clone(), version.clone()))
            .map(String::as_str)
            .unwrap_or_default();
        let mut notices = notice_files(source)?
            .iter()
            .filter_map(|f| fs::read_to_string(f).ok())
            .map(|t| t.lines().map(normalized).collect::<Vec<_>>().join(" "))
            .collect::<Vec<_>>()
            .join(" ");
        notices.push_str(carried);
        let missing: Vec<_> = copyright_lines(source)?
            .into_iter()
            .filter(|(notice, _)| !notices.contains(&flattened(notice)))
            .collect();
        if missing.is_empty() {
            continue;
        }
        section.push_str(&format!("\n{name} {version}\n"));
        for (line, files) in missing {
            section.push('\n');
            for text in line.split('\n') {
                section.push_str(&format!("  {text}\n"));
            }
            section.push_str(&format!("    in {}\n", files.join(", ")));
        }
    }
    if section.is_empty() {
        return Ok(section);
    }
    Ok(format!(
        "\n{rule}\nAdditional copyright notices\n\n\
         Copyright notices in the sources of these crates that the license texts above\n\
         do not carry, with the files that hold them. The files are under the license\n\
         the notices name for their crate.\n{section}",
        rule = "-".repeat(80),
    ))
}

/// Fails unless the notices carry every copyright line of every crate's
/// sources that `copyright_lines` counts.
fn check_copyright_lines(crates: &[(String, String, PathBuf)], text: &str) -> Result<()> {
    let notices = text.lines().map(normalized).collect::<Vec<_>>().join(" ");
    let mut missing = Vec::new();
    for (name, version, source) in crates {
        for (line, files) in copyright_lines(source)? {
            if !notices.contains(&flattened(&line)) {
                missing.push(format!("{name} {version}, {}: {line}", files.join(", ")));
            }
        }
    }
    if !missing.is_empty() {
        bail!(
            "the notices lack copyright lines of the crates' sources:\n{}",
            missing.join("\n")
        );
    }
    Ok(())
}

/// Whether a line states a copyright notice. These forms count:
///
/// - `SPDX-FileCopyrightText`, anywhere in the line;
/// - the word `copyright`, in any letter case, followed by a year, `©`, `ⓒ`
///   or `(c)` in either case, as in clap's `Copyright ⓒ 2015-2016 …`;
/// - a line that starts, past its comment markers and an optional
///   `Portions` in any letter case, with `copyright` in any letter case, an
///   optional `:`, and a holder, a word that starts with a capital letter,
///   as in `Copyright The Rust Project Developers` or `copyright Example
///   Author.`, unless that word is one of license prose, such as
///   `COPYRIGHT HOLDERS` or `copyright notice`;
/// - a line that starts with `©` or `ⓒ`.
fn is_copyright_line(line: &str) -> bool {
    const PROSE: &[&str] = &[
        "notice",
        "notices",
        "holder",
        "holders",
        "owner",
        "owners",
        "license",
        "licenses",
        "law",
        "laws",
        "act",
        "statement",
        "and",
        "or",
    ];
    if line.contains("SPDX-FileCopyrightText") {
        return true;
    }
    let lower = line.to_lowercase();
    let symbol_or_year = |rest: &str| {
        let rest = rest.trim_start();
        rest.starts_with(|c: char| c.is_ascii_digit())
            || rest.starts_with("(c)")
            || rest.starts_with('©')
            || rest.starts_with('ⓒ')
    };
    let noticed = lower.match_indices("copyright").any(|(at, word)| {
        let before = lower[..at].chars().next_back();
        let after = lower[at + word.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric)
            && !after.is_some_and(char::is_alphanumeric)
            && symbol_or_year(&lower[at + word.len()..])
    });
    if noticed {
        return true;
    }
    let text = normalized(line);
    let text = strip_prefix_ignore_case(&text, "portions ").unwrap_or(&text);
    if text.starts_with('©') || text.starts_with('ⓒ') {
        return true;
    }
    let Some(rest) = strip_prefix_ignore_case(text, "copyright") else {
        return false;
    };
    if !rest.starts_with([' ', ':']) {
        return false;
    }
    let holder = rest.trim_start_matches([' ', ':']);
    let word: String = holder.chars().take_while(|c| c.is_alphanumeric()).collect();
    word.starts_with(char::is_uppercase) && !PROSE.contains(&word.to_lowercase().as_str())
}

/// `text` without the ASCII `prefix`, compared in ASCII case.
fn strip_prefix_ignore_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &text[prefix.len()..])
}

/// A line without the comment markers before it, with its whitespace
/// collapsed.
fn normalized(line: &str) -> String {
    let line = line.trim_start_matches(|c: char| c.is_whitespace() || "/*#;!@-".contains(c));
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Every file under `dir`, sorted by path.
fn source_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(&dir).with_context(|| format!("cannot list {}", dir.display()))? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                dirs.push(entry.path());
            } else if kind.is_file() {
                files.push(entry.path());
            }
        }
    }
    files.sort();
    Ok(files)
}

/// The sysroot of the Rust toolchain that builds the binary.
fn rust_sysroot() -> Result<PathBuf> {
    let out = Process::new("rustc")
        .args(["--print", "sysroot"])
        .output()
        .context("cannot run rustc")?;
    if !out.status.success() {
        bail!("rustc --print sysroot failed with {}", out.status);
    }
    Ok(PathBuf::from(String::from_utf8(out.stdout)?.trim()))
}

/// The notices of the Rust toolchain's code in the binary: compiler-builtins,
/// with its libm, from the toolchain's rust-src component, and a pointer to
/// the copy of COPYRIGHT-library.html.
fn rust_notices(sysroot: &Path) -> Result<String> {
    let out = Process::new("rustc")
        .arg("--version")
        .output()
        .context("cannot run rustc")?;
    let rustc = String::from_utf8(out.stdout)?.trim().to_string();
    let builtins = sysroot.join("lib/rustlib/src/rust/library/compiler-builtins");
    let read = |file: &str| -> Result<String> {
        let path = builtins.join(file);
        fs::read_to_string(&path).with_context(|| {
            format!(
                "cannot read {}; the notices need the toolchain's rust-src component \
                 (`rustup component add rust-src`)",
                path.display()
            )
        })
    };
    Ok(format!(
        "\n{rule}\nThe Rust standard library and compiler-builtins ({rustc})\n\n\
         The binary links the Rust standard library of the toolchain that built it. The\n\
         notices of the standard library, and of the crates it is built from, are in\n\
         {RUST_LIBRARY_COPYRIGHT}, the toolchain's COPYRIGHT-library.html. The\n\
         compiler-builtins crate it also links, with its libm, is under these terms:\n\n\
         {}\n\n{rule}\nlibm of compiler-builtins ({rustc})\n\n{}\n",
        read("LICENSE.txt")?.trim_end(),
        read("libm/LICENSE.txt")?.trim_end(),
        rule = "-".repeat(80),
    ))
}

/// For a Linux (musl) target, musl's COPYRIGHT and the Zig distribution's
/// LICENSE, from the zig installation that cargo-zigbuild links the binary
/// with: cargo-zigbuild leaves out the toolchain's own musl, and zig links the
/// musl it ships, its own libc (libzigc) and its compiler_rt statically. For
/// any other target, which zig does not link, nothing, without running zig.
fn zig_notices(target: &str) -> Result<String> {
    if !target.ends_with("-linux-musl") {
        return Ok(String::new());
    }
    let zig = std::env::var_os("CARGO_ZIGBUILD_ZIG_PATH").unwrap_or_else(|| "zig".into());
    let out = Process::new(&zig)
        .arg("env")
        .output()
        .context("cannot run zig, which links the release binaries")?;
    if !out.status.success() {
        bail!("zig env failed with {}", out.status);
    }
    let env = String::from_utf8(out.stdout)?;
    let field = |name: &str| -> Result<String> {
        env.lines()
            .find_map(|l| l.trim().strip_prefix(&format!(".{name} = \"")))
            .and_then(|v| v.strip_suffix("\","))
            .map(str::to_string)
            .with_context(|| format!("zig env names no {name}"))
    };
    let lib_dir = PathBuf::from(field("lib_dir")?);
    let zig_version = field("version")?;
    // Zig's own libc and compiler_rt, which it builds from its lib directory,
    // are under the license of the Zig distribution, at the root of the
    // installation.
    let zig_license = lib_dir
        .parent()
        .map(|root| root.join("LICENSE"))
        .context("zig's lib directory has no parent")?;
    let zig_license = fs::read_to_string(&zig_license)
        .with_context(|| format!("cannot read zig's license {}", zig_license.display()))?;
    let rule = "-".repeat(80);
    let musl = lib_dir.join("libc/musl");
    let header = fs::read_to_string(musl.join("src/internal/version.h"))
        .with_context(|| format!("cannot read musl's version in {}", musl.display()))?;
    let version = header
        .lines()
        .find_map(|l| l.strip_prefix("#define VERSION \""))
        .and_then(|v| v.strip_suffix('"'))
        .context("musl's version.h names no version")?;
    let copyright = fs::read_to_string(musl.join("COPYRIGHT"))
        .with_context(|| format!("cannot read musl's COPYRIGHT in {}", musl.display()))?;
    Ok(format!(
        "\n{rule}\nmusl libc {version} (linked statically, as Zig {zig_version} ships it)\n\n{}\n\
         \n{rule}\nZig {zig_version} (its libc and compiler_rt, linked statically)\n\n{}\n",
        copyright.trim_end(),
        zig_license.trim_end(),
    ))
}

/// The files at a crate's root whose name starts with `NOTICE`, in any letter
/// case, sorted by name.
fn notice_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("cannot list {}", dir.display()))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_ascii_uppercase();
        if name.starts_with("NOTICE") && entry.file_type()?.is_file() {
            files.push(entry.path());
        }
    }
    files.sort();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notices_accept_what_cargo_deny_allows() {
        check_accepted(Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap()).unwrap();
    }

    /// Only the Linux (musl) targets are linked by zig; the notices of a
    /// macOS target are made without running zig at all.
    #[test]
    fn zig_notices_are_only_for_the_musl_targets() {
        for target in ["aarch64-apple-darwin", "x86_64-apple-darwin"] {
            assert_eq!(zig_notices(target).unwrap(), "", "{target}");
        }
    }

    #[test]
    fn copyright_lines_are_recognised_in_every_common_form() {
        for line in [
            // The form of clap 4.6.7's and clap_builder 4.6.7's src/lib.rs.
            "// Copyright ⓒ 2015-2016 Example Authors and [`example` contributors](https://example.invalid/).",
            "// Copyright 2016 Example Authors.",
            "/* Copyright (c) 2014, Intel Corporation.",
            "# Copyright (C) 2019 Example Authors",
            "Copyright © 2005-2020 Example Authors, et al.",
            "© 2014 Unicode, Inc.",
            "// Copyright The Rust Project Developers",
            "Copyright: Example Project contributors",
            "// Portions Copyright (c) 2016, Google Inc.",
            "// SPDX-FileCopyrightText: 2023 Example Authors",
            "COPYRIGHT 2001 EXAMPLE CORP.",
            // The form of serde_json 1.0.151's src/lexical/mod.rs, line 9.
            "// copyright Example Author.",
        ] {
            assert!(is_copyright_line(line), "{line}");
        }
        for line in [
            "// The above copyright notice and this permission notice shall be included in",
            "// IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM,",
            "COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER",
            "// copyright, or license notices",
            "Copyright and related rights waived via CC0",
            "// characters such as '#' and '©'.",
            "let copyright = read();",
            "// copyright notice and this permission notice appear in all copies.",
        ] {
            assert!(!is_copyright_line(line), "{line}");
        }
    }

    #[test]
    fn a_notice_keeps_the_lines_that_continue_its_holders() {
        let header = "// Copyright 2018 Example Author A (@a),\n\
                      // Example Author B (@b), and\n\
                      // Example Author C (@c)\n\
                      //\n\
                      // Licensed under the Apache License, Version 2.0\n\
                      // Copyright 2019 D.\n\
                      fn main() {}\n";
        assert_eq!(
            copyright_notices(header),
            [
                "Copyright 2018 Example Author A (@a),\n\
                 Example Author B (@b), and\n\
                 Example Author C (@c)",
                "Copyright 2019 D.",
            ]
        );
    }

    /// The locked serde_json's lowercase notice and clap_derive's notice of
    /// three lines are among the copyright notices of their sources, whole.
    #[test]
    fn the_locked_lowercase_and_multi_line_notices_are_kept() {
        let source = |name: &str| {
            let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
            let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
            let out = Process::new(cargo)
                .current_dir(root)
                .args(["metadata", "--locked", "--format-version", "1"])
                .output()
                .unwrap();
            let metadata: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
            let manifest = metadata["packages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["name"] == name)
                .and_then(|p| p["manifest_path"].as_str())
                .unwrap()
                .to_string();
            PathBuf::from(manifest).parent().unwrap().to_path_buf()
        };
        let line = |dir: &Path, file: &str, number: usize| {
            let text = fs::read_to_string(dir.join(file)).unwrap();
            normalized(text.lines().nth(number - 1).unwrap())
        };
        let serde_json = source("serde_json");
        let notice = line(&serde_json, "src/lexical/mod.rs", 9);
        assert!(notice.starts_with("copyright "), "{notice}");
        let notices = copyright_lines(&serde_json).unwrap();
        assert!(notices.contains_key(&notice), "{notices:?}");
        let clap_derive = source("clap_derive");
        let header = (1..=3)
            .map(|n| line(&clap_derive, "src/lib.rs", n))
            .collect::<Vec<_>>()
            .join("\n");
        let notices = copyright_lines(&clap_derive).unwrap();
        assert!(notices.contains_key(&header), "{header}\n{notices:?}");
    }

    /// The locked clap's header, which writes `Copyright ⓒ`, is among the
    /// copyright lines of its sources.
    #[test]
    fn the_locked_clap_header_is_a_copyright_line() {
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let out = Process::new(cargo)
            .current_dir(root)
            .args(["metadata", "--locked", "--format-version", "1"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let metadata: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        for name in ["clap", "clap_builder"] {
            let manifest = metadata["packages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["name"] == name)
                .and_then(|p| p["manifest_path"].as_str())
                .unwrap();
            let source = Path::new(manifest).parent().unwrap();
            let header = fs::read_to_string(source.join("src/lib.rs")).unwrap();
            let header = header.lines().next().unwrap();
            assert!(is_copyright_line(header), "{name}: {header}");
            let lines = copyright_lines(source).unwrap();
            assert!(
                lines
                    .get(&normalized(header))
                    .is_some_and(|files| files.iter().any(|f| f == "src/lib.rs")),
                "{name}: {lines:?}"
            );
        }
    }
}
