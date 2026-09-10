//! Sorting behaviour adapted from cleandl-rs (MIT, CleanDL contributors).
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Datelike, Utc};
use filetime::FileTime;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, Metadata, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::SystemTime,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Rule {
    pub regex: String,
    pub destination: Vec<String>,
    #[serde(default)]
    pub order: i64,
    #[serde(default)]
    pub append_year_month: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Extension {
    pub name: String,
    #[serde(default)]
    pub default_destination: Vec<String>,
    #[serde(default)]
    pub append_year_month: bool,
    #[serde(default)]
    pub rules: Vec<Rule>,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Config {
    pub settings: HashMap<String, String>,
    pub ignore_rules: Vec<String>,
    pub delete_rules: Vec<String>,
    pub extensions: Vec<Extension>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Move(PathBuf),
    Recycle,
    Skip(String),
}
impl Action {
    pub fn label(&self) -> &str {
        match self {
            Self::Move(_) => "Move",
            Self::Recycle => "Recycle",
            Self::Skip(_) => "Skip",
        }
    }
    pub fn actionable(&self) -> bool {
        !matches!(self, Self::Skip(_))
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stamp {
    pub size: u64,
    modified: SystemTime,
    created: SystemTime,
    // Captured from a handle, not a lossy path string.
    identity: (u32, u64),
}
impl Stamp {
    fn read(path: &Path) -> Result<Self> {
        let m = fs::symlink_metadata(path)?;
        if !m.is_file() || m.file_type().is_symlink() {
            bail!("Source is not a regular file");
        }
        let file = File::open(path)?;
        Self::from_file(&file)
    }
    fn from_file(file: &File) -> Result<Self> {
        let m = file.metadata()?;
        Ok(Self {
            size: m.len(),
            modified: m.modified()?,
            created: m.created()?,
            identity: crate::platform::file_identity(file)?,
        })
    }
}
#[derive(Clone, Debug)]
pub struct Item {
    pub source: PathBuf,
    pub size: u64,
    pub action: Action,
    pub status: String,
    stamp: Option<Stamp>,
}
#[derive(Clone, Debug)]
pub struct Plan {
    pub items: Vec<Item>,
    pub source: PathBuf,
    pub destination: PathBuf,
    pub cached: bool,
    pub trial: bool,
    pub demo: bool,
    pub warnings: Vec<String>,
}
impl Plan {
    pub fn counts(&self) -> (usize, usize) {
        (
            self.items
                .iter()
                .filter(|i| matches!(i.action, Action::Move(_)))
                .count(),
            self.items
                .iter()
                .filter(|i| i.action == Action::Recycle)
                .count(),
        )
    }
}
#[derive(Debug)]
pub enum Event {
    Phase(String),
    Progress {
        index: usize,
        total: usize,
        bytes: u64,
        size: u64,
        name: String,
    },
    Item {
        index: usize,
        status: String,
    },
}
#[derive(Default, Debug)]
pub struct Summary {
    pub completed: usize,
    pub skipped: usize,
    pub failed: usize,
    pub cancelled: bool,
}

pub fn cancelled(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        bail!("Cancelled");
    }
    Ok(())
}
pub fn scan(source: &Path, cancel: &AtomicBool) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in
        fs::read_dir(source).with_context(|| format!("Cannot scan {}", source.display()))?
    {
        cancelled(cancel)?;
        let entry = entry?;
        if entry.file_type()?.is_file() {
            paths.push(entry.path());
        }
    }
    paths.sort_by_key(|p| path_key(p));
    Ok(paths)
}
pub fn extension_names(paths: &[PathBuf]) -> HashSet<String> {
    paths
        .iter()
        .filter_map(|p| p.extension().map(|e| e.to_string_lossy().to_lowercase()))
        .collect()
}
fn path_key(p: &Path) -> String {
    p.to_string_lossy().to_lowercase()
}
fn compile(pattern: &str, warnings: &mut Vec<String>) -> Option<Regex> {
    match Regex::new(pattern) {
        Ok(r) => Some(r),
        Err(e) => {
            warnings.push(format!("Invalid rule {pattern}: {e}"));
            None
        }
    }
}

pub fn preview(
    config: &Config,
    paths: Vec<PathBuf>,
    source: PathBuf,
    destination: PathBuf,
    cancel: &AtomicBool,
) -> Result<Plan> {
    let mut plan = Plan {
        items: Vec::new(),
        source,
        destination,
        cached: false,
        trial: false,
        demo: false,
        warnings: Vec::new(),
    };
    let delete: Vec<_> = config
        .delete_rules
        .iter()
        .filter_map(|p| compile(p, &mut plan.warnings))
        .collect();
    let ignore: Vec<_> = config
        .ignore_rules
        .iter()
        .filter_map(|p| compile(p, &mut plan.warnings))
        .collect();
    let mut extensions = HashMap::new();
    for ext in &config.extensions {
        let mut rules = ext.rules.clone();
        rules.sort_by_key(|r| r.order);
        let rules: Vec<_> = rules
            .into_iter()
            .filter_map(|r| compile(&r.regex, &mut plan.warnings).map(|regex| (regex, r)))
            .collect();
        extensions.insert(ext.name.to_lowercase(), (ext, rules));
    }
    let mut reserved = HashSet::new();
    for path in paths {
        cancelled(cancel)?;
        let mut item = Item {
            source: path.clone(),
            size: 0,
            action: Action::Skip(String::new()),
            status: "Ready".into(),
            stamp: None,
        };
        let result = (|| -> Result<Action> {
            let stamp = Stamp::read(&path)
                .with_context(|| format!("Cannot inspect source {}", path.display()))?;
            item.size = stamp.size;
            item.stamp = Some(stamp);
            let name = path
                .file_name()
                .context("Missing filename")?
                .to_string_lossy();
            if delete.iter().any(|r| r.is_match(&name)) {
                return Ok(Action::Recycle);
            }
            if ignore.iter().any(|r| r.is_match(&name)) {
                return Ok(Action::Skip("Ignored by rule".into()));
            }
            let ext = path
                .extension()
                .map(|s| s.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let Some((mapping, rules)) = extensions.get(&ext) else {
                return Ok(Action::Skip("No active extension mapping".into()));
            };
            let (parts, dated) = rules.iter().find(|(re, _)| re.is_match(&name)).map_or(
                (&mapping.default_destination, mapping.append_year_month),
                |(_, r)| (&r.destination, r.append_year_month),
            );
            if parts.is_empty() {
                return Ok(Action::Skip("Empty destination".into()));
            }
            let mut dir = plan.destination.clone();
            for part in parts {
                dir.push(part);
            }
            if dated {
                let stamp = item.stamp.as_ref().context("Missing source metadata")?;
                let date: DateTime<Utc> = stamp.modified.into();
                dir.push(date.year().to_string());
                dir.push(format!("{:02}", date.month()));
            }
            let mut target = dir.join(path.file_name().context("Missing filename")?);
            if path_key(&target) == path_key(&path) {
                return Ok(Action::Skip("Already at destination".into()));
            }
            if reserved.contains(&path_key(&target))
                || target
                    .try_exists()
                    .with_context(|| format!("Cannot check destination {}", target.display()))?
            {
                if !reserved.contains(&path_key(&target))
                    && fs::metadata(&target).is_ok_and(|m| m.is_file() && m.len() == item.size)
                {
                    return Ok(Action::Skip("Destination exists with the same size".into()));
                }
                let stem = path.file_stem().unwrap_or_default().to_string_lossy();
                let suffix = path
                    .extension()
                    .map(|e| format!(".{}", e.to_string_lossy()))
                    .unwrap_or_default();
                let mut available = None;
                for n in 1..=10_000 {
                    let candidate = dir.join(format!("{stem}-{n}{suffix}"));
                    if !reserved.contains(&path_key(&candidate))
                        && !candidate.try_exists().with_context(|| {
                            format!("Cannot check destination {}", candidate.display())
                        })?
                    {
                        available = Some(candidate);
                        break;
                    }
                }
                target = available.context("Too many destination collisions")?;
            }
            reserved.insert(path_key(&target));
            Ok(Action::Move(target))
        })();
        match result {
            Ok(action) => {
                if let Action::Skip(reason) = &action {
                    item.status.clone_from(reason);
                }
                item.action = action;
            }
            Err(e) => {
                item.status = format!("Cannot plan: {e:#}");
                item.action = Action::Skip(item.status.clone());
            }
        }
        plan.items.push(item);
    }
    Ok(plan)
}

pub fn apply(plan: &Plan, cancel: &AtomicBool, emit: &mut impl FnMut(Event)) -> Result<Summary> {
    apply_with_recycler(plan, cancel, emit, &mut crate::platform::recycle)
}
#[derive(Debug)]
struct NeedsPreview(&'static str);
impl std::fmt::Display for NeedsPreview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}; preview again", self.0)
    }
}
impl std::error::Error for NeedsPreview {}
fn apply_with_recycler(
    plan: &Plan,
    cancel: &AtomicBool,
    emit: &mut impl FnMut(Event),
    recycle: &mut impl FnMut(&Path) -> Result<()>,
) -> Result<Summary> {
    if plan.demo || plan.trial {
        bail!("This preview is trial-only; files cannot be changed");
    }
    let mut summary = Summary::default();
    for (index, item) in plan.items.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            summary.cancelled = true;
            break;
        }
        if !item.action.actionable() {
            summary.skipped += 1;
            continue;
        }
        let status = match Stamp::read(&item.source) {
            Ok(stamp) if Some(&stamp) == item.stamp.as_ref() => {
                emit(Event::Progress {
                    index,
                    total: plan.items.len(),
                    bytes: 0,
                    size: item.size,
                    name: item.source.display().to_string(),
                });
                let result = match &item.action {
                    Action::Move(dst) => copy_item(item, dst, cancel, |bytes| {
                        emit(Event::Progress {
                            index,
                            total: plan.items.len(),
                            bytes,
                            size: item.size,
                            name: item.source.display().to_string(),
                        })
                    }),
                    Action::Recycle => recycle(&item.source).map(|()| None),
                    Action::Skip(_) => unreachable!(),
                };
                match result {
                    Ok(warning) => {
                        summary.completed += 1;
                        warning.unwrap_or_else(|| "Completed".into())
                    }
                    Err(e) if e.downcast_ref::<NeedsPreview>().is_some() => {
                        summary.skipped += 1;
                        format!("Skipped: {e}")
                    }
                    Err(e) => {
                        summary.failed += 1;
                        format!("Failed: {e:#}")
                    }
                }
            }
            _ => {
                summary.skipped += 1;
                "Skipped: source changed or is unavailable; preview again".into()
            }
        };
        emit(Event::Item { index, status });
    }
    summary.cancelled |= cancel.load(Ordering::Relaxed);
    Ok(summary)
}
fn copy_item(
    item: &Item,
    dst: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64),
) -> Result<Option<String>> {
    cancelled(cancel)?;
    if dst.try_exists()? {
        return Err(NeedsPreview("Destination became occupied").into());
    }
    let mut input = crate::platform::open_source_locked(&item.source)?;
    if Some(&Stamp::from_file(&input)?) != item.stamp.as_ref() {
        return Err(NeedsPreview("Source changed").into());
    }
    let metadata = input.metadata()?;
    fs::create_dir_all(dst.parent().context("Missing destination directory")?)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dst)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                anyhow::Error::new(NeedsPreview("Destination became occupied"))
            } else {
                anyhow::Error::new(e).context("Cannot exclusively create reviewed destination")
            }
        })?;
    let result =
        copy_stream(&mut input, &mut output, item.size, cancel, &mut progress).and_then(|()| {
            output.sync_all()?;
            Ok(())
        });
    drop(output);
    if let Err(error) = result {
        return match fs::remove_file(dst) {
            Ok(()) => Err(error),
            Err(cleanup) => {
                Err(error.context(format!("Partial destination cleanup failed: {cleanup}")))
            }
        };
    }
    let warning = preserve_times(dst, &metadata)
        .err()
        .map(|e| format!("Completed; timestamp warning: {e}"));
    // The locked source handle prevents replacement or writes during the copy.
    // Delete through that handle so a path replacement cannot delete another file.
    crate::platform::delete_open_source(&input)
        .context("Copied, but source removal failed (both copies retained)")?;
    Ok(warning)
}
fn preserve_times(dst: &Path, metadata: &Metadata) -> Result<()> {
    filetime::set_file_times(
        dst,
        FileTime::from_last_access_time(metadata),
        FileTime::from_last_modification_time(metadata),
    )?;
    Ok(())
}
fn copy_stream(
    input: &mut impl Read,
    output: &mut impl Write,
    expected: u64,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(u64),
) -> Result<()> {
    let mut buffer = vec![0; 1024 * 1024];
    let mut copied = 0u64;
    loop {
        cancelled(cancel)?;
        let n = match input.read(&mut buffer) {
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            other => other?,
        };
        if n == 0 {
            break;
        }
        output.write_all(&buffer[..n])?;
        copied += n as u64;
        progress(copied);
    }
    if copied != expected {
        bail!("Source size changed or copy was incomplete");
    }
    Ok(())
}

pub fn demo_config() -> Config {
    let mut cfg = Config {
        ignore_rules: vec![r"(?i)\.(crdownload|download|part)$".into()],
        delete_rules: vec![r"(?i)\.(temp|tmp)$".into()],
        ..Config::default()
    };
    for (names, destination, dated) in [
        ("doc docx odt rtf txt", "Documents", false),
        ("csv ods xls xlsx", "Documents/Spreadsheets", false),
        ("mp3 flac m4a ogg wav", "Music", false),
        ("mkv mov mp4 webm", "Videos", true),
        ("7z bz2 gz rar tar xz zip", "Archives", false),
        ("deb dmg exe iso msi pkg rpm", "Software", false),
        ("gif jpeg jpg png webp", "Pictures", false),
        ("pdf", "Documents/PDF", false),
    ] {
        for name in names.split_whitespace() {
            let mut ext = Extension {
                name: name.into(),
                default_destination: destination.split('/').map(str::to_owned).collect(),
                append_year_month: dated,
                rules: Vec::new(),
            };
            let rules = if name == "pdf" {
                vec![
                    ("(?i)(invoice|receipt)", "Documents/Finance"),
                    ("(?i)^(scan|scanned)", "Documents/Scanned"),
                ]
            } else if destination == "Pictures" {
                vec![("(?i)^screenshot", "Pictures/Screenshots")]
            } else {
                Vec::new()
            };
            for (order, (regex, path)) in rules.into_iter().enumerate() {
                ext.rules.push(Rule {
                    regex: regex.into(),
                    destination: path.split('/').map(str::to_owned).collect(),
                    order: order as i64,
                    append_year_month: true,
                });
            }
            cfg.extensions.push(ext);
        }
    }
    cfg
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    fn fixture() -> (TempDir, PathBuf, PathBuf) {
        let t = TempDir::new().unwrap();
        let src = t.path().join("source");
        let dst = t.path().join("destination");
        fs::create_dir(&src).unwrap();
        (t, src, dst)
    }
    fn plan(src: &Path, dst: &Path) -> Plan {
        preview(
            &demo_config(),
            scan(src, &AtomicBool::new(false)).unwrap(),
            src.into(),
            dst.into(),
            &AtomicBool::new(false),
        )
        .unwrap()
    }
    #[test]
    fn preview_is_read_only_and_routes() {
        let (_t, src, dst) = fixture();
        for name in ["invoice.pdf", "screen.png", "a.tmp", "a.part", "unknown"] {
            fs::write(src.join(name), b"hello").unwrap();
        }
        let p = plan(&src, &dst);
        assert!(!dst.exists());
        assert_eq!(p.counts(), (2, 1));
        assert!(p.items.iter().any(
            |i| matches!(&i.action, Action::Move(p) if p.to_string_lossy().contains("Finance"))
        ));
        assert_eq!(fs::read_dir(src).unwrap().count(), 5);
    }
    #[test]
    fn changed_sources_and_destination_races_are_safe() {
        let (_t, src, dst) = fixture();
        fs::write(src.join("a.txt"), b"a").unwrap();
        fs::write(src.join("b.txt"), b"b").unwrap();
        let p = plan(&src, &dst);
        fs::write(src.join("a.txt"), b"changed").unwrap();
        fs::create_dir_all(dst.join("Documents")).unwrap();
        fs::write(dst.join("Documents/b.txt"), b"unrelated").unwrap();
        let result = apply(&p, &AtomicBool::new(false), &mut |_| {}).unwrap();
        assert_eq!(result.skipped, 2);
        assert_eq!(result.failed, 0);
        assert_eq!(fs::read(dst.join("Documents/b.txt")).unwrap(), b"unrelated");
        assert!(src.join("b.txt").exists());
    }
    #[test]
    fn moves_empty_and_large_files_and_preserves_mtime() {
        let (_t, src, dst) = fixture();
        fs::write(src.join("a.txt"), b"").unwrap();
        fs::write(src.join("b.txt"), vec![42; 2_100_000]).unwrap();
        let time = FileTime::from_unix_time(1_600_000_000, 0);
        filetime::set_file_mtime(src.join("b.txt"), time).unwrap();
        let p = plan(&src, &dst);
        assert_eq!(
            apply(&p, &AtomicBool::new(false), &mut |_| {})
                .unwrap()
                .completed,
            2
        );
        assert!(!src.join("b.txt").exists());
        assert_eq!(
            fs::metadata(dst.join("Documents/b.txt")).unwrap().len(),
            2_100_000
        );
        assert_eq!(
            FileTime::from_last_modification_time(
                &fs::metadata(dst.join("Documents/b.txt")).unwrap()
            ),
            time
        );
    }
    #[test]
    fn cancellation_removes_partial_copy() {
        let (_t, src, dst) = fixture();
        fs::write(src.join("a.txt"), vec![42; 2_100_000]).unwrap();
        let p = plan(&src, &dst);
        let cancel = AtomicBool::new(false);
        let result = apply(&p, &cancel, &mut |e| {
            if matches!(e, Event::Progress { bytes, .. } if bytes > 0) {
                cancel.store(true, Ordering::Relaxed);
            }
        })
        .unwrap();
        assert!(result.cancelled);
        assert!(src.join("a.txt").exists());
        assert!(!dst.join("Documents/a.txt").exists());
    }
    #[test]
    fn recycling_failure_never_deletes_and_trial_is_enforced() {
        let (_t, src, dst) = fixture();
        fs::write(src.join("a.tmp"), b"keep").unwrap();
        let mut p = plan(&src, &dst);
        let summary = apply_with_recycler(&p, &AtomicBool::new(false), &mut |_| {}, &mut |_| {
            bail!("Unavailable")
        })
        .unwrap();
        assert_eq!(summary.failed, 1);
        assert!(src.join("a.tmp").exists());
        p.demo = true;
        assert!(apply(&p, &AtomicBool::new(false), &mut |_| {}).is_err());
        p.demo = false;
        p.trial = true;
        assert!(apply(&p, &AtomicBool::new(false), &mut |_| {}).is_err());
    }
    #[test]
    fn delete_precedes_ignore_and_collisions_suffix() {
        let (_t, src, dst) = fixture();
        fs::write(src.join("a.txt"), b"abc").unwrap();
        fs::create_dir_all(dst.join("Documents")).unwrap();
        fs::write(dst.join("Documents/a.txt"), b"x").unwrap();
        let p = plan(&src, &dst);
        assert!(matches!(&p.items[0].action, Action::Move(p) if p.ends_with("a-1.txt")));
        let mut cfg = demo_config();
        cfg.ignore_rules = vec![".*".into()];
        cfg.delete_rules = vec![".*".into()];
        let p = preview(
            &cfg,
            scan(&src, &AtomicBool::new(false)).unwrap(),
            src,
            dst,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(p.items[0].action, Action::Recycle);
    }
    #[test]
    fn ordered_rules_use_utc_date_and_ignore_subfolders() {
        let (_t, src, dst) = fixture();
        fs::create_dir(src.join("nested")).unwrap();
        fs::write(src.join("nested/leave.txt"), b"keep").unwrap();
        let file = src.join("Résumé invoice.PDF");
        fs::write(&file, b"invoice").unwrap();
        filetime::set_file_mtime(&file, FileTime::from_unix_time(1_609_459_200, 0)).unwrap();
        let mut cfg = demo_config();
        let ext = cfg.extensions.iter_mut().find(|e| e.name == "pdf").unwrap();
        ext.rules.insert(
            0,
            Rule {
                regex: ".*".into(),
                destination: vec!["Wrong".into()],
                order: 99,
                append_year_month: false,
            },
        );
        let p = preview(
            &cfg,
            scan(&src, &AtomicBool::new(false)).unwrap(),
            src,
            dst.clone(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(p.items.len(), 1);
        assert_eq!(
            p.items[0].action,
            Action::Move(dst.join("Documents/Finance/2021/01/Résumé invoice.PDF"))
        );
    }
    #[test]
    fn failure_does_not_block_later_moves_and_new_files_wait() {
        let (_t, src, dst) = fixture();
        for name in ["a.pdf", "b.txt"] {
            fs::write(src.join(name), b"abc").unwrap();
        }
        let p = plan(&src, &dst);
        fs::create_dir_all(dst.join("Documents")).unwrap();
        // A file where a destination directory is needed forces a real failure.
        fs::write(dst.join("Documents/PDF"), b"unrelated").unwrap();
        fs::write(src.join("new.txt"), b"new").unwrap();
        let summary = apply(&p, &AtomicBool::new(false), &mut |_| {}).unwrap();
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.completed, 1);
        assert!(src.join("new.txt").exists());
        assert!(!src.join("b.txt").exists());
    }
    #[test]
    fn short_and_failed_writes_are_errors() {
        let cancel = AtomicBool::new(false);
        assert!(copy_stream(&mut &b"a"[..], &mut Vec::new(), 2, &cancel, &mut |_| {}).is_err());
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("disk full"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        assert!(copy_stream(&mut &b"a"[..], &mut Broken, 1, &cancel, &mut |_| {}).is_err());
    }
}
