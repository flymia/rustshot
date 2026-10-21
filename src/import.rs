//! `rustshot import` - bulk import screenshots from the filesystem.

use std::path::{Path, PathBuf};

use anyhow::Context;
use clap::Args;
use time::OffsetDateTime;

use crate::db;
use crate::image;
use crate::service::{Ctx, NewScreenshot, UploadError};
use crate::size::ByteSize;

#[derive(Args)]
pub struct ImportOpts {
    /// User the screenshots are imported for
    username: String,
    /// Files and/or directories to import
    #[arg(required = true)]
    paths: Vec<PathBuf>,
    /// Descend into subdirectories
    #[arg(short, long)]
    recursive: bool,
    /// Skip files this user already uploaded (same SHA-256)
    #[arg(long)]
    skip_duplicates: bool,
    /// Date uploads with the import time instead of the file's modification time
    #[arg(long)]
    use_current_time: bool,
    /// Only report what would be imported
    #[arg(long)]
    dry_run: bool,
}

pub async fn run(ctx: &Ctx, opts: ImportOpts) -> anyhow::Result<()> {
    let user = crate::cli::require_user(ctx, &opts.username).await?;
    let files = collect_files(&opts.paths, opts.recursive)?;

    let (mut imported, mut skipped, mut bytes) = (0usize, 0usize, 0u64);
    for path in files {
        let data = tokio::fs::read(&path)
            .await
            .with_context(|| format!("reading {}", path.display()))?;

        if let Err(e) = image::inspect(&data) {
            println!("skip    {} ({e})", path.display());
            skipped += 1;
            continue;
        }
        if opts.skip_duplicates {
            let hash = crate::ids::sha256_hex(&data);
            if let Some(existing) = db::find_upload_by_hash(&ctx.db, &user.id, &hash).await? {
                println!(
                    "skip    {} (duplicate of {})",
                    path.display(),
                    ctx.share_url(&existing)
                );
                skipped += 1;
                continue;
            }
        }
        if opts.dry_run {
            println!("would   {}", path.display());
            imported += 1;
            bytes += data.len() as u64;
            continue;
        }

        let created_at = if opts.use_current_time {
            None
        } else {
            modified_time(&path).await
        };
        let new = NewScreenshot {
            data: &data,
            original_name: path.file_name().map(|n| n.to_string_lossy().into_owned()),
            created_at,
        };
        match ctx.store_screenshot(&user, new).await {
            Ok(upload) => {
                println!("import  {} -> {}", path.display(), ctx.share_url(&upload));
                imported += 1;
                bytes += data.len() as u64;
            }
            Err(UploadError::QuotaExceeded) => {
                anyhow::bail!(
                    "quota of {} exceeded at {} ({imported} files imported so far)",
                    user.username,
                    path.display()
                );
            }
            Err(e) => return Err(e).with_context(|| format!("importing {}", path.display())),
        }
    }

    let verb = if opts.dry_run { "would import" } else { "imported" };
    println!("{verb} {imported} files ({}), skipped {skipped}", ByteSize(bytes));
    Ok(())
}

async fn modified_time(path: &Path) -> Option<OffsetDateTime> {
    let modified = tokio::fs::metadata(path).await.ok()?.modified().ok()?;
    Some(OffsetDateTime::from(modified))
}

/// Expands the given paths into a sorted list of regular files.
/// Hidden files and directories (dot-prefixed) inside directories are ignored.
fn collect_files(paths: &[PathBuf], recursive: bool) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for path in paths {
        let meta = std::fs::metadata(path).with_context(|| format!("reading {}", path.display()))?;
        if meta.is_dir() {
            walk_dir(path, recursive, &mut files)?;
        } else {
            files.push(path.clone());
        }
    }
    files.sort();
    files.dedup();
    Ok(files)
}

fn walk_dir(dir: &Path, recursive: bool, files: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            if recursive {
                walk_dir(&entry.path(), recursive, files)?;
            }
        } else if file_type.is_file() {
            files.push(entry.path());
        }
    }
    Ok(())
}
