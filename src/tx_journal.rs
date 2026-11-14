//! Crash-safe multi-file transaction journal and recovery.
//!
//! Commits several staged filesystem writes using a two-phase write-ahead
//! protocol (`PREPARED` -> `COMMITTING` -> `COMMITTED`). Temporary new files and
//! backups of pre-existing destinations are written and synced beside each
//! destination before the WAL is marked `COMMITTING`, so:
//!
//! - Failures before or during replacement restore every modified file to its
//!   pre-transaction contents before returning.
//! - A crash at any point leaves either no `.wal` / `PREPARED` (original files
//!   untouched), `COMMITTING` (recovery rolls back every entry using backups),
//!   or `COMMITTED` (recovery removes backup/WAL artifacts).
//!
//! Note that single-file `rename` / `MoveFileEx` is OS-atomic only within one
//! mount/volume; placing each temporary/backup next to its destination keeps
//! per-file swaps within the destination's filesystem, while the WAL binds all
//! entries into one crash-recoverable commit or rollback protocol.

use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub const WAL_HEADER: &str = "IRIS_TX_WAL/1";
static NEXT_TX_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalState {
    Prepared,
    Committing,
    Committed,
}

impl WalState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "PREPARED",
            Self::Committing => "COMMITTING",
            Self::Committed => "COMMITTED",
        }
    }

    fn parse(raw: &str) -> std::io::Result<Self> {
        match raw.trim() {
            "PREPARED" => Ok(Self::Prepared),
            "COMMITTING" => Ok(Self::Committing),
            "COMMITTED" => Ok(Self::Committed),
            other => Err(std::io::Error::new(
                ErrorKind::InvalidData,
                format!("unknown IRIS transaction WAL state '{other}'"),
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalEntry {
    pub existed: bool,
    pub dest: PathBuf,
    pub new_path: PathBuf,
    pub bak_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalTransaction {
    pub state: WalState,
    pub entries: Vec<WalEntry>,
}

fn effective_parent(path: &Path) -> PathBuf {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

fn sync_parent_dir(path: &Path) {
    #[cfg(unix)]
    {
        if let Ok(dir) = File::open(effective_parent(path)) {
            let _ = dir.sync_all();
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

fn write_synced_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.flush()?;
    file.sync_all()?;
    Ok(())
}

fn copy_synced_file(src: &Path, dst: &Path) -> std::io::Result<()> {
    let mut reader = File::open(src)?;
    let mut writer = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(dst)?;
    let mut buf = [0u8; 8192];
    loop {
        let read = reader.read(&mut buf)?;
        if read == 0 {
            break;
        }
        writer.write_all(&buf[..read])?;
    }
    writer.flush()?;
    writer.sync_all()?;
    Ok(())
}

fn encode_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn decode_hex(hex: &str) -> std::io::Result<String> {
    let raw = hex.as_bytes();
    if raw.len() % 2 != 0 {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "odd-length hex field in IRIS transaction WAL",
        ));
    }
    let mut out = Vec::with_capacity(raw.len() / 2);
    for pair in raw.chunks_exact(2) {
        let s = std::str::from_utf8(pair).map_err(|_| {
            std::io::Error::new(ErrorKind::InvalidData, "non-ASCII hex field in WAL")
        })?;
        let byte = u8::from_str_radix(s, 16)
            .map_err(|_| std::io::Error::new(ErrorKind::InvalidData, "invalid hex digit in WAL"))?;
        out.push(byte);
    }
    String::from_utf8(out)
        .map_err(|_| std::io::Error::new(ErrorKind::InvalidData, "non-UTF-8 path in WAL"))
}

pub fn serialize_wal(wal: &WalTransaction) -> String {
    let mut text = String::new();
    let _ = writeln!(text, "{WAL_HEADER}");
    let _ = writeln!(text, "state={}", wal.state.as_str());
    let _ = writeln!(text, "count={}", wal.entries.len());
    for entry in &wal.entries {
        let existed = u8::from(entry.existed);
        let dest = encode_hex(entry.dest.to_string_lossy().as_bytes());
        let new_path = encode_hex(entry.new_path.to_string_lossy().as_bytes());
        let bak_path = encode_hex(entry.bak_path.to_string_lossy().as_bytes());
        let _ = writeln!(text, "entry={existed}|{dest}|{new_path}|{bak_path}");
    }
    text
}

pub fn parse_wal(text: &str) -> std::io::Result<WalTransaction> {
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let header = lines
        .next()
        .ok_or_else(|| std::io::Error::new(ErrorKind::InvalidData, "empty IRIS transaction WAL"))?;
    if header.trim() != WAL_HEADER {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            format!("unexpected IRIS transaction WAL header '{header}'"),
        ));
    }
    let state_line = lines
        .next()
        .ok_or_else(|| std::io::Error::new(ErrorKind::InvalidData, "missing state line in WAL"))?;
    let state = WalState::parse(state_line.strip_prefix("state=").ok_or_else(|| {
        std::io::Error::new(ErrorKind::InvalidData, "malformed state line in WAL")
    })?)?;
    let count_line = lines
        .next()
        .ok_or_else(|| std::io::Error::new(ErrorKind::InvalidData, "missing count line in WAL"))?;
    let count: usize = count_line
        .strip_prefix("count=")
        .ok_or_else(|| std::io::Error::new(ErrorKind::InvalidData, "malformed count line in WAL"))?
        .trim()
        .parse()
        .map_err(|_| std::io::Error::new(ErrorKind::InvalidData, "invalid count in WAL"))?;

    let mut entries = Vec::with_capacity(count);
    for line in lines {
        let payload = line.strip_prefix("entry=").ok_or_else(|| {
            std::io::Error::new(ErrorKind::InvalidData, "malformed entry line in WAL")
        })?;
        let parts: Vec<&str> = payload.split('|').collect();
        if parts.len() != 4 {
            return Err(std::io::Error::new(
                ErrorKind::InvalidData,
                "invalid entry column count in WAL",
            ));
        }
        let existed = match parts[0] {
            "1" => true,
            "0" => false,
            other => {
                return Err(std::io::Error::new(
                    ErrorKind::InvalidData,
                    format!("invalid existed flag '{other}' in WAL"),
                ))
            }
        };
        entries.push(WalEntry {
            existed,
            dest: PathBuf::from(decode_hex(parts[1])?),
            new_path: PathBuf::from(decode_hex(parts[2])?),
            bak_path: PathBuf::from(decode_hex(parts[3])?),
        });
    }
    if entries.len() != count {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            format!(
                "WAL count mismatch: expected {count} entries, found {}",
                entries.len()
            ),
        ));
    }
    Ok(WalTransaction { state, entries })
}

fn atomic_write_wal(wal_path: &Path, wal: &WalTransaction) -> std::io::Result<()> {
    let tmp_name = format!(
        "{}.tmp",
        wal_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "iris-txn.wal".to_owned())
    );
    let tmp_path = wal_path.with_file_name(tmp_name);
    let serialized = serialize_wal(wal);
    write_synced_file(&tmp_path, serialized.as_bytes())?;
    std::fs::rename(&tmp_path, wal_path)?;
    sync_parent_dir(wal_path);
    Ok(())
}

fn replace_file_durable(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::rename(src, dst)?;
    sync_parent_dir(dst);
    Ok(())
}

fn sibling_artifact(dest: &Path, tag: &str, pid: u32, txid: u64, index: usize) -> Option<PathBuf> {
    let file_name = dest.file_name()?;
    let name = format!(
        ".{}.iris-txn-{tag}-{pid}-{txid}-{index}",
        file_name.to_string_lossy()
    );
    Some(dest.with_file_name(name))
}

fn rollback_entries(entries: &[WalEntry]) {
    for entry in entries.iter().rev() {
        if entry.existed {
            if entry.bak_path.exists() {
                let _ = replace_file_durable(&entry.bak_path, &entry.dest);
            }
        } else {
            let _ = std::fs::remove_file(&entry.dest);
            sync_parent_dir(&entry.dest);
        }
    }
}

fn cleanup_wal_artifacts(wal_path: Option<&Path>, entries: &[WalEntry]) {
    for entry in entries {
        let _ = std::fs::remove_file(&entry.new_path);
        let _ = std::fs::remove_file(&entry.bak_path);
    }
    if let Some(wal) = wal_path {
        let _ = std::fs::remove_file(wal);
        let tmp_name = format!(
            "{}.tmp",
            wal.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "iris-txn.wal".to_owned())
        );
        let _ = std::fs::remove_file(wal.with_file_name(tmp_name));
        sync_parent_dir(wal);
    }
}

/// Commit all staged files using the crash-safe two-phase WAL protocol.
///
/// On any failure before `COMMITTED` is durably persisted, all destination files
/// are restored to their exact pre-transaction state before returning `Err`.
pub fn commit_staged_files(staged_files: &[(PathBuf, Vec<u8>)]) -> Result<(), String> {
    if staged_files.is_empty() {
        return Ok(());
    }

    // Phase 0: Security and structural preflight before writing any temp file.
    for (dest, bytes) in staged_files {
        if dest.file_name().is_none() {
            return Err(format!(
                "staged path '{}' has no destination file name",
                dest.display()
            ));
        }
        crate::security::check_fs_write_bytes(&dest.to_string_lossy(), bytes.len() as u64)
            .map_err(|e| e.to_string())?;
        if let Ok(meta) = std::fs::symlink_metadata(dest) {
            if meta.is_dir() {
                return Err(format!(
                    "cannot commit file over directory '{}'",
                    dest.display()
                ));
            }
        }
    }

    let pid = std::process::id();
    let txid = NEXT_TX_ID.fetch_add(1, Ordering::Relaxed);
    let mut entries = Vec::with_capacity(staged_files.len());

    // Phase 1: Stage and sync `.new` files beside every destination.
    for (index, (dest, bytes)) in staged_files.iter().enumerate() {
        let new_path = sibling_artifact(dest, "new", pid, txid, index)
            .ok_or_else(|| format!("invalid destination path '{}'", dest.display()))?;
        let bak_path = sibling_artifact(dest, "bak", pid, txid, index)
            .ok_or_else(|| format!("invalid destination path '{}'", dest.display()))?;
        if let Err(err) = write_synced_file(&new_path, bytes) {
            cleanup_wal_artifacts(None, &entries);
            let _ = std::fs::remove_file(&new_path);
            return Err(format!(
                "failed to stage temporary file '{}': {err}",
                new_path.display()
            ));
        }
        entries.push(WalEntry {
            existed: false,
            dest: dest.clone(),
            new_path,
            bak_path,
        });
    }

    // Phase 2: Snapshot pre-existing destination files into `.bak` files.
    for index in 0..entries.len() {
        let dest = entries[index].dest.clone();
        let bak_path = entries[index].bak_path.clone();
        match std::fs::symlink_metadata(&dest) {
            Ok(meta) => {
                if meta.is_dir() {
                    cleanup_wal_artifacts(None, &entries);
                    return Err(format!("cannot overwrite directory '{}'", dest.display()));
                }
                entries[index].existed = true;
                if let Err(err) = copy_synced_file(&dest, &bak_path) {
                    cleanup_wal_artifacts(None, &entries);
                    return Err(format!(
                        "failed to snapshot backup '{}': {err}",
                        bak_path.display()
                    ));
                }
            }
            Err(err) if err.kind() == ErrorKind::NotFound => {
                entries[index].existed = false;
            }
            Err(err) => {
                cleanup_wal_artifacts(None, &entries);
                return Err(format!(
                    "failed to inspect destination '{}': {err}",
                    dest.display()
                ));
            }
        }
    }

    // Phase 3 & 4: Write `PREPARED` WAL then durably transition to `COMMITTING`.
    let wal_path = effective_parent(&entries[0].dest).join(format!(".iris-txn-{pid}-{txid}.wal"));
    let mut wal = WalTransaction {
        state: WalState::Prepared,
        entries,
    };
    if let Err(err) = atomic_write_wal(&wal_path, &wal) {
        cleanup_wal_artifacts(Some(&wal_path), &wal.entries);
        return Err(format!("failed to write PREPARED WAL: {err}"));
    }

    wal.state = WalState::Committing;
    if let Err(err) = atomic_write_wal(&wal_path, &wal) {
        cleanup_wal_artifacts(Some(&wal_path), &wal.entries);
        return Err(format!("failed to write COMMITTING WAL: {err}"));
    }

    // Phase 5: Replace destinations; roll back all modified entries on error.
    for (idx, entry) in wal.entries.iter().enumerate() {
        if let Err(err) = replace_file_durable(&entry.new_path, &entry.dest) {
            rollback_entries(&wal.entries[..idx]);
            cleanup_wal_artifacts(Some(&wal_path), &wal.entries);
            return Err(format!(
                "atomic replace failed on '{}': {err}",
                entry.dest.display()
            ));
        }
    }

    // Phase 6: Mark `COMMITTED` before clearing backups so a crash before
    // cleanup retains committed outputs.
    wal.state = WalState::Committed;
    if let Err(err) = atomic_write_wal(&wal_path, &wal) {
        rollback_entries(&wal.entries);
        cleanup_wal_artifacts(Some(&wal_path), &wal.entries);
        return Err(format!("failed to finalize COMMITTED WAL: {err}"));
    }
    cleanup_wal_artifacts(Some(&wal_path), &wal.entries);
    Ok(())
}

/// Replay or roll back one WAL file left on disk by an interrupted transaction.
pub fn recover_wal_file(wal_path: &Path) -> Result<WalState, String> {
    let text = std::fs::read_to_string(wal_path)
        .map_err(|e| format!("failed to read WAL '{}': {e}", wal_path.display()))?;
    let wal = parse_wal(&text)
        .map_err(|e| format!("failed to parse WAL '{}': {e}", wal_path.display()))?;
    for entry in &wal.entries {
        crate::security::check_fs_write(&entry.dest.to_string_lossy())
            .map_err(|e| e.to_string())?;
    }
    match wal.state {
        WalState::Prepared | WalState::Committed => {}
        WalState::Committing => rollback_entries(&wal.entries),
    }
    cleanup_wal_artifacts(Some(wal_path), &wal.entries);
    Ok(wal.state)
}

/// Scan `dir` for incomplete `.iris-txn-*.wal` journals and recover them.
///
/// Returns the number of WAL journals processed.
pub fn recover_incomplete_transactions(dir: &Path) -> Result<usize, String> {
    crate::security::check_fs_write(&dir.to_string_lossy()).map_err(|e| e.to_string())?;
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(0),
        Err(err) => {
            return Err(format!(
                "failed to scan directory '{}' for WAL files: {err}",
                dir.display()
            ))
        }
    };
    let mut recovered = 0usize;
    for item in entries {
        let item = item.map_err(|e| format!("failed reading directory entry: {e}"))?;
        let name = item.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with(".iris-txn-") && name_str.ends_with(".wal") {
            recover_wal_file(&item.path())?;
            recovered += 1;
        }
    }
    Ok(recovered)
}
