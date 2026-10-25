use std::{
    collections::HashSet,
    env,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

pub(crate) const MUSE_DATA_DIR_ENV: &str = "MUSE_DATA_DIR";
const XDG_DATA_HOME_ENV: &str = "XDG_DATA_HOME";
pub(super) const MUSE_SESSIONS_DIR_NAME: &str = "sessions";
const MUSE_SESSION_FILE_NAME: &str = "session.jsonl";
/// Directories inside a session that never hold a session log. Muse persists
/// large tool results and attachments here; agents can also write files with
/// arbitrary names into them, so they are not walked at all.
const SKIPPED_DIR_NAMES: &[&str] = &["tool-outputs", "approval-review", "input-assets"];

/// Muse data roots, each the directory that contains `sessions/`.
///
/// A non-empty `MUSE_DATA_DIR` holds comma-separated roots and replaces the
/// default; roots that are not directories are ignored rather than falling back
/// to the default. Otherwise the root is `$XDG_DATA_HOME/muse` when
/// `XDG_DATA_HOME` is an absolute path, else `~/.local/share/muse`, which is
/// where Muse itself writes sessions.
pub(super) fn data_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let mut seen = HashSet::new();
    if let Ok(raw) = env::var(MUSE_DATA_DIR_ENV)
        && !raw.trim().is_empty()
    {
        for path in raw
            .split(',')
            .map(str::trim)
            .filter(|path| !path.is_empty())
        {
            push_unique_dir(&mut roots, &mut seen, PathBuf::from(path));
        }
        return roots;
    }
    if let Some(root) = default_data_root() {
        push_unique_dir(&mut roots, &mut seen, root);
    }
    roots
}

fn default_data_root() -> Option<PathBuf> {
    if let Some(xdg) = env::var_os(XDG_DATA_HOME_ENV)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        return Some(xdg.join("muse"));
    }
    crate::home::home_dir().map(|home| home.join(".local/share/muse"))
}

fn push_unique_dir(roots: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>, path: PathBuf) {
    if !path.is_dir() {
        return;
    }
    let key = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
    if seen.insert(key) {
        roots.push(path);
    }
}

/// Each data root's `sessions/` directory that exists, for diagnostics such as
/// `turbotokens doctor`.
pub fn sessions_dirs() -> Vec<PathBuf> {
    data_roots()
        .into_iter()
        .map(|root| root.join(MUSE_SESSIONS_DIR_NAME))
        .filter(|dir| dir.is_dir())
        .collect()
}

/// Every `session.jsonl` under each root's `sessions/` tree, sorted.
///
/// Main sessions live at `sessions/YYYY/MM/DD/<session>/session.jsonl` and
/// subagent sessions at `<session>/subagent/<child>/session.jsonl`; both are
/// usage logs. Hidden directories (Muse's `.msp-view-*` binary journals and
/// `.spool` scratch files) and the tool-output/attachment directories are
/// skipped, as is every file not named exactly `session.jsonl`.
pub(super) fn discover_session_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    for root in data_roots() {
        collect_session_files(&root.join(MUSE_SESSIONS_DIR_NAME), &mut files);
    }
    files.sort();
    files.dedup();
    files
}

pub(super) fn collect_session_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(std::result::Result::ok) {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let name = entry.file_name();
        if file_type.is_dir() {
            if !is_skipped_dir(&name) {
                collect_session_files(&entry.path(), files);
            }
        } else if file_type.is_file() && name == MUSE_SESSION_FILE_NAME {
            files.push(entry.path());
        }
    }
}

fn is_skipped_dir(name: &OsStr) -> bool {
    name.as_encoded_bytes().starts_with(b".")
        || SKIPPED_DIR_NAMES.iter().any(|skipped| name == *skipped)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;
    use turbotokens_test_support::{EnvVarsGuard, fs_fixture};

    fn env_for(data_dir: Option<OsString>, xdg: Option<OsString>, home: &Path) -> EnvVarsGuard {
        EnvVarsGuard::set_many([
            (MUSE_DATA_DIR_ENV, data_dir),
            (XDG_DATA_HOME_ENV, xdg),
            ("HOME", Some(home.as_os_str().to_os_string())),
            ("USERPROFILE", None),
            ("HOMEDRIVE", None),
            ("HOMEPATH", None),
        ])
    }

    #[test]
    fn discovers_main_and_subagent_logs_and_skips_hidden_and_tool_dirs() {
        let fixture = fs_fixture!({
            "muse/sessions/2026/10/24/s1/session.jsonl": "{}\n",
            "muse/sessions/2026/10/24/s1/subagent/c1/session.jsonl": "{}\n",
            "muse/sessions/2026/10/24/s1/cli-1.log": "{}\n",
            "muse/sessions/2026/10/24/s1/session.peer-history.sqlite3": "",
            "muse/sessions/2026/10/24/s1/tool-outputs/session.jsonl": "{}\n",
            "muse/sessions/2026/10/24/s1/tool-outputs/.spool/call-1.txt.tmp": "",
            "muse/sessions/2026/10/24/s1/approval-review/session.jsonl": "{}\n",
            "muse/sessions/2026/10/24/s1/input-assets/session.jsonl": "{}\n",
            "muse/sessions/2026/10/24/s1/other.jsonl": "{}\n",
            "muse/sessions/.msp-view-g6/s1/session.jsonl": "{}\n",
            "muse/sessions/.msp-view-g6/s1/journal-0.bin": "",
            "muse/sessions/2026/10/24/.hidden/session.jsonl": "{}\n",
            "muse/tui-history.jsonl": "{}\n",
        });
        let _env = env_for(
            Some(fixture.path("muse").into_os_string()),
            None,
            fixture.root(),
        );

        assert_eq!(
            discover_session_files(),
            vec![
                fixture.path("muse/sessions/2026/10/24/s1/session.jsonl"),
                fixture.path("muse/sessions/2026/10/24/s1/subagent/c1/session.jsonl"),
            ]
        );
    }

    #[test]
    fn data_dir_accepts_comma_separated_roots_and_deduplicates_them() {
        let fixture = fs_fixture!({
            "a/sessions/2026/10/24/s1/session.jsonl": "{}\n",
            "b/sessions/2026/10/24/s2/session.jsonl": "{}\n",
        });
        let raw = format!(
            " {}, {}, {}, {} ",
            fixture.path("a").display(),
            fixture.path("b").display(),
            fixture.path("a").display(),
            fixture.path("missing").display()
        );
        let _env = env_for(Some(OsString::from(raw)), None, fixture.root());

        assert_eq!(
            data_roots(),
            vec![fixture.path("a"), fixture.path("b")],
            "duplicates and missing roots are dropped"
        );
        assert_eq!(discover_session_files().len(), 2);
    }

    #[test]
    fn invalid_data_dir_does_not_fall_back_to_the_default_root() {
        let fixture = fs_fixture!({
            ".local/share/muse/sessions/2026/10/24/s1/session.jsonl": "{}\n",
        });
        let _env = env_for(
            Some(fixture.path("missing").into_os_string()),
            None,
            fixture.root(),
        );

        assert!(discover_session_files().is_empty());
    }

    #[test]
    fn defaults_to_xdg_data_home_then_local_share() {
        let fixture = fs_fixture!({
            "xdg/muse/sessions/2026/10/24/s1/session.jsonl": "{}\n",
            ".local/share/muse/sessions/2026/10/24/s2/session.jsonl": "{}\n",
        });
        {
            let _env = env_for(
                Some(OsString::new()),
                Some(fixture.path("xdg").into_os_string()),
                fixture.root(),
            );
            assert_eq!(
                discover_session_files(),
                vec![fixture.path("xdg/muse/sessions/2026/10/24/s1/session.jsonl")]
            );
        }
        {
            // A relative XDG_DATA_HOME is invalid per the XDG spec and ignored.
            let _env = env_for(None, Some(OsString::from("relative/xdg")), fixture.root());
            assert_eq!(
                discover_session_files(),
                vec![fixture.path(".local/share/muse/sessions/2026/10/24/s2/session.jsonl")]
            );
        }
    }
}
