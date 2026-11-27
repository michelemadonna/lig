//! Contextual diffs use Git's raw object reader. Worktree bytes are read by Lig,
//! so repository attributes cannot dispatch external diff or clean filters.
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use similar::TextDiff;

use super::{Config, content, external, native, target::Git};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Diff {
    Unified,
    SideBySide,
}

fn installed_tool(name: &str, source: &Path, search_path: Option<&str>) -> Option<PathBuf> {
    let path = search_path.map(std::ffi::OsString::from).or_else(|| std::env::var_os("PATH"))?;
    let name = if cfg!(windows) { format!("{name}.exe") } else { name.to_owned() };
    std::env::split_paths(&path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(&name))
        .find(|path| path != source && crate::engine::command_name(path).is_some())
}

pub fn preview(
    mode: Git,
    request: &super::Request,
    config: &Config,
    max_lines: usize,
    cancelled: &dyn Fn() -> bool,
) -> Option<(content::Text, Option<Diff>)> {
    let path = &request.path;
    let cwd = &request.cwd;
    let search_path = request.search_path.as_deref();
    let relative = path.strip_prefix(cwd).ok()?;
    if relative.components().any(|part| !matches!(part, std::path::Component::Normal(_))) {
        return None;
    }
    let limit = config.max_bytes.min(131_072);
    let timeout = Duration::from_millis(config.timeout_ms);
    let executable = installed_tool("git", path, search_path)?;
    let git = |args: &[&str]| {
        let mut cmd = Command::new(&executable);
        cmd.current_dir(cwd)
            .env("GIT_NO_LAZY_FETCH", "1")
            .args(["--no-pager", "--no-optional-locks", "-c", "core.fsmonitor=false"])
            .args(args);
        external::run(cmd, None, limit, timeout, cancelled).ok()
    };
    let prefix = git(&["rev-parse", "--show-prefix"])?;
    let mut name = String::from_utf8(prefix).ok()?.trim_end_matches(['\n', '\r']).to_string();
    name.push_str(&relative.to_str()?.replace(std::path::MAIN_SEPARATOR, "/"));
    let object = |revision: &str| {
        // Check type before reading. A gitlink or a tree is not text.
        (git(&["cat-file", "-t", revision])?.trim_ascii() == b"blob")
            .then(|| git(&["cat-file", "blob", revision]))
            .flatten()
    };
    let index = object(&format!(":{name}"));
    let staged = || {
        let head = object(&format!("HEAD:{name}"));
        (head.is_some() || index.is_some()).then(|| (head.unwrap_or_default(), index.clone().unwrap_or_default()))
    };
    let (before, after) = if mode == Git::Staged {
        staged()?
    } else {
        let before = index.as_ref()?;
        let mut after = Vec::new();
        if path.is_file() {
            native::open_file(path).ok()?.take(limit as u64 + 1).read_to_end(&mut after).ok()?;
            if after.len() > limit {
                return None;
            }
        } else if path.exists() {
            return None;
        }
        if mode == Git::Add && *before == after { staged()? } else { (before.clone(), after) }
    };
    if cancelled() || before.contains(&0) || after.contains(&0) {
        return None;
    }
    let (before, after) = (String::from_utf8_lossy(&before), String::from_utf8_lossy(&after));
    if before == after {
        return Some((content::Text::plain("No changes for this tracked path", max_lines), None));
    }
    let diff = TextDiff::configure().timeout(timeout.min(Duration::from_millis(100))).diff_lines(&before, &after);
    let label = crate::popup::printable(&name);
    let text = diff.unified_diff().header(&format!("a/{label}"), &format!("b/{label}")).to_string();
    if let Some(width) = request.git_width.filter(|width| *width >= super::layout::MIN_DIFF_COLUMNS)
        && let Some(executable) = installed_tool("delta", path, search_path)
    {
        let mut delta = Command::new(executable);
        delta.current_dir(cwd).arg("--side-by-side").arg("--paging=never").arg(format!("--width={width}"));
        if let Ok(bytes) = external::run(delta, Some(text.as_bytes()), config.max_bytes, timeout, cancelled)
            && let Some(text) = content::external(&bytes, max_lines, true)
        {
            return Some((text, Some(Diff::SideBySide)));
        }
    }
    let mut output = content::TextBuilder::new(max_lines, false);
    for line in text.lines().take(max_lines) {
        let fg = if line.starts_with('+') {
            Some(content::Color::Index(2))
        } else if line.starts_with('-') {
            Some(content::Color::Index(1))
        } else {
            None
        };
        output.push(line, content::Style { fg, ..Default::default() });
        output.newline();
    }
    Some((output.finish(), Some(Diff::Unified)))
}
