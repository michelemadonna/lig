use std::path::{Path, PathBuf};

pub fn file_candidate(item: &crate::engine::Suggestion) -> bool {
    use crate::engine::Kind;
    item.back == 0
        && (matches!(item.kind, Kind::File | Kind::Folder)
            || (item.kind == Kind::Arg && matches!(item.insert.trim(), "." | "..")))
}

pub fn file_from_candidate(line: &str, item: &crate::engine::Suggestion, cwd: &Path) -> Option<PathBuf> {
    use crate::{engine::is_sep, parser};
    if !file_candidate(item) {
        return None;
    }
    let token = parser::parse(line).current;
    let original = token.value;
    // The engine replaces only the basename for native filesystem candidates. Parse
    // the inserted value separately for generator candidates whose label is decorative.
    let raw_count = token.raw.chars().count();
    if item.delete > raw_count {
        return None;
    }
    let mut raw: String = token.raw.chars().take(raw_count - item.delete).collect();
    let expands_home = original.strip_prefix('~').is_some_and(|suffix| suffix.starts_with(is_sep));
    raw.push_str(&item.insert);
    let parsed = parser::parse(&format!("__lig_preview {raw}"));
    if parsed.tokens.first().is_none_or(|command| command.value != "__lig_preview") {
        return None;
    }
    let mut values =
        parsed.tokens.iter().skip(1).chain(std::iter::once(&parsed.current)).filter(|value| !value.raw.is_empty());
    let value = values.next()?.value.as_str();
    if values.next().is_some() || value.is_empty() || value.contains('\0') {
        return None;
    }
    // A parsed value that dropped the typed directory can't safely address the candidate.
    if item.delete < raw_count && original.contains(is_sep) && !value.starts_with(&original[..=original.rfind(is_sep)?])
    {
        return None;
    }
    if (expands_home || raw.starts_with('~'))
        && value.strip_prefix('~').is_some_and(|suffix| suffix.starts_with(is_sep))
    {
        return Some(dirs::home_dir()?.join(&value[2..]));
    }
    let path = cwd.join(value);
    if !path.is_absolute() {
        return None;
    }
    #[cfg(windows)]
    if path.components().next().is_some_and(|c| matches!(c,
        std::path::Component::Prefix(p) if matches!(p.kind(), std::path::Prefix::DeviceNS(_) | std::path::Prefix::Verbatim(_))))
    {
        return None;
    }
    Some(path)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Git {
    Worktree,
    Staged,
    Add,
}

pub fn git_context(
    line: &str,
    completion_command: &str,
    aliases: &std::collections::HashMap<String, String>,
) -> Option<Git> {
    if completion_command != "git" {
        return None;
    }
    let parsed = crate::parser::parse(line);
    let first = parsed.tokens.first()?;
    let alias = aliases.get(&first.value).map(|s| crate::parser::parse(&format!("{s} ")));
    let mut words = alias
        .iter()
        .flat_map(|a| &a.tokens)
        .chain(parsed.tokens.iter().skip(usize::from(alias.is_some())))
        .map(|token| token.value.as_str());
    words.find(|word| {
        Path::new(word)
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|name| name == "git" || (cfg!(windows) && name.eq_ignore_ascii_case("git.exe")))
    })?;
    let command = words.next()?;
    if !matches!(command, "add" | "diff" | "restore") {
        return None;
    }
    let staged = words
        .take_while(|w| *w != "--")
        .any(|w| matches!(w, "--cached" | "--staged") || (command == "restore" && w == "-S"));
    Some(if staged {
        Git::Staged
    } else if command == "add" {
        Git::Add
    } else {
        Git::Worktree
    })
}
