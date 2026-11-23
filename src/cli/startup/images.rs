//! `horon images`: read-only inventory of the images one session generated.
//!
//! The session log is the only source of ownership. A stored `generate_image`
//! result lists its files as ``- `path` (N bytes, mime)``; only those lines are
//! read, and a file counts only if it is a regular file directly inside
//! `generated-images/`. Nothing is created, moved or uploaded.

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::cli::args::ImagesArgs;
use crate::message::ContentBlock;

/// The model-facing name of the image tool, as stored in a session log.
const IMAGE_TOOL: &str = "generate_image";

pub(super) fn run(args: &ImagesArgs) -> Result<()> {
    let session_id = crate::session::find_session_by_name_or_id(&args.session)?;
    let session = crate::session::Session::load(&session_id)
        .with_context(|| format!("cannot read session {:?}", args.session))?;
    let history = session.history_messages()?;
    let logged = collect(history.iter().flat_map(|message| message.content.iter()));
    let dir = crate::storage::horon_agent_dir()?.join("generated-images");
    let images = inspect_all(&logged, &dir);
    if let Some(copy) = &args.copy {
        return copy_one(&images, &copy[0], Path::new(&copy[1]));
    }
    report(&session_id, &images, args.json, io::stdout().lock())
}

#[derive(Debug, PartialEq, Eq)]
struct Logged {
    path: PathBuf,
    bytes: u64,
    mime_type: String,
}

#[derive(Debug, Serialize)]
struct Image {
    number: usize,
    path: PathBuf,
    mime_type: String,
    bytes: u64,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
}

/// Every image the session's `generate_image` calls reported, in call order.
fn collect<'a>(blocks: impl Iterator<Item = &'a ContentBlock>) -> Vec<Logged> {
    let mut image_calls = HashSet::new();
    let mut found = Vec::new();
    for block in blocks {
        match block {
            ContentBlock::ToolUse { id, name, .. } if name == IMAGE_TOOL => {
                image_calls.insert(id.as_str());
            }
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
                ..
            } if image_calls.contains(tool_use_id.as_str()) && *is_error != Some(true) => {
                found.extend(content.lines().filter_map(parse_line));
            }
            _ => {}
        }
    }
    found
}

fn parse_line(line: &str) -> Option<Logged> {
    let rest = line.strip_prefix("- `")?;
    let (path, rest) = rest.split_once("` (")?;
    let (bytes, mime_type) = rest.strip_suffix(')')?.split_once(" bytes, ")?;
    let Ok(bytes) = bytes.parse() else { return None };
    Some(Logged {
        path: PathBuf::from(path),
        bytes,
        mime_type: mime_type.to_owned(),
    })
}

fn inspect_all(logged: &[Logged], dir: &Path) -> Vec<Image> {
    let dir = dir.canonicalize().into_iter().next();
    logged
        .iter()
        .enumerate()
        .map(|(index, logged)| {
            let checked = inspect(&logged.path, dir.as_deref());
            Image {
                number: index + 1,
                path: logged.path.clone(),
                mime_type: logged.mime_type.clone(),
                bytes: checked.as_ref().copied().unwrap_or(logged.bytes),
                status: if checked.is_ok() { "ok" } else { "refused" },
                reason: checked.err(),
            }
        })
        .collect()
}

/// The file's size, or why it is not an image this session may expose.
fn inspect(path: &Path, dir: Option<&Path>) -> Result<u64, &'static str> {
    let dir = dir.ok_or("the generated-images directory does not exist")?;
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err("the path is not a plain absolute path");
    }
    if !matches!(path.parent().map(Path::canonicalize), Some(Ok(parent)) if parent == dir) {
        return Err("the file is outside the generated-images directory");
    }
    let metadata = std::fs::symlink_metadata(path).map_err(|_| "the file is missing")?;
    if !metadata.is_file() {
        return Err("the path is a symbolic link or not a regular file");
    }
    Ok(metadata.len())
}

fn report(session: &str, images: &[Image], json: bool, mut out: impl Write) -> Result<()> {
    if json {
        serde_json::to_writer_pretty(&mut out, &serde_json::json!({ "session": session, "images": images }))?;
        writeln!(out)?;
        return Ok(());
    }
    if images.is_empty() {
        writeln!(out, "No generated images recorded for this session.")?;
    }
    for image in images {
        match image.reason {
            None => writeln!(
                out,
                "{}. {} ({}, {} bytes)",
                image.number,
                image.path.display(),
                image.mime_type,
                image.bytes
            )?,
            Some(reason) => writeln!(out, "{}. refused: {reason}", image.number)?,
        }
    }
    Ok(())
}

fn copy_one(images: &[Image], number: &str, dest: &Path) -> Result<()> {
    let wanted: usize = number
        .parse()
        .with_context(|| format!("{number:?} is not an image number"))?;
    let image = images
        .iter()
        .find(|image| image.number == wanted)
        .with_context(|| format!("no image number {wanted} in this session"))?;
    if let Some(reason) = image.reason {
        anyhow::bail!("image {} was refused: {reason}", image.number);
    }
    let mut source = File::open(&image.path).context("cannot open the image")?;
    let mut target = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dest)
        .with_context(|| format!("cannot create {} (it must not exist yet)", dest.display()))?;
    io::copy(&mut source, &mut target).context("the copy failed")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(id: &str, name: &str) -> ContentBlock {
        ContentBlock::ToolUse {
            id: id.to_owned(),
            name: name.to_owned(),
            input: serde_json::json!({}),
            thought_signature: None,
        }
    }

    fn result(id: &str, text: &str) -> ContentBlock {
        ContentBlock::ToolResult {
            tool_use_id: id.to_owned(),
            content: text.to_owned(),
            is_error: None,
            verdict: None,
        }
    }

    #[test]
    fn only_image_tool_results_list_files() {
        let blocks = [
            call("a", IMAGE_TOOL),
            result("a", "Generated 1 image.\n- `/x/one.png` (12 bytes, image/png)\nDone."),
            call("b", "bash"),
            result("b", "- `/x/two.png` (9 bytes, image/png)"),
        ];
        assert_eq!(
            collect(blocks.iter()),
            vec![Logged {
                path: PathBuf::from("/x/one.png"),
                bytes: 12,
                mime_type: "image/png".to_owned()
            }]
        );
    }

    #[cfg(unix)]
    #[test]
    fn outside_symlinked_and_escaping_paths_are_refused() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("generated-images");
        std::fs::create_dir(&dir).unwrap();
        let inside = dir.join("a.png");
        std::fs::write(&inside, b"12345").unwrap();
        let outside = root.path().join("secret.png");
        std::fs::write(&outside, b"secret").unwrap();
        let link = dir.join("link.png");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let logged = |path: PathBuf| Logged {
            path,
            bytes: 1,
            mime_type: "image/png".to_owned(),
        };
        let images = inspect_all(
            &[
                logged(inside),
                logged(outside),
                logged(link),
                logged(dir.join("../secret.png")),
                logged(dir.join("missing.png")),
            ],
            &dir,
        );
        let states: Vec<_> = images.iter().map(|image| (image.status, image.bytes)).collect();
        assert_eq!(
            states,
            [
                ("ok", 5),
                ("refused", 1),
                ("refused", 1),
                ("refused", 1),
                ("refused", 1)
            ]
        );
        let dest = root.path().join("copy.png");
        assert!(copy_one(&images, "2", &dest).is_err());
        assert!(!dest.exists());
        copy_one(&images, "1", &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"12345");
        assert!(
            copy_one(&images, "1", &dest).is_err(),
            "an existing file is never overwritten"
        );
    }
}
