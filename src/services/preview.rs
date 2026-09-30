// SPDX-License-Identifier: MIT

use std::{
    borrow::Cow,
    ffi::OsStr,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

use crate::model::FileEntry;

use super::{DocumentLayout, LoadHandle, operations::ArchiveFormat};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PreviewRequestId(pub u64);

/// How much detail a raster preview asks for. `Standard` fits the drawer;
/// `Expanded` is for a larger presentation.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum PreviewDetail {
    #[default]
    Standard,
    /// `width` is the width of the larger presentation in device pixels.
    Expanded { width: i32 },
}

impl PreviewDetail {
    pub fn is_expanded(self) -> bool {
        matches!(self, Self::Expanded { .. })
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct MediaPreviewSize {
    pub width: i32,
    pub height: i32,
    /// Sized for the expanded view: as large as it is shown, scaled more carefully.
    pub expanded: bool,
    /// The most frames per second to decode: 30 in the drawer, and up to the
    /// screen's refresh rate (60) in the expanded view.
    pub max_fps: u32,
}

impl MediaPreviewSize {
    pub const MAX_EDGE: i32 = 1280;
    /// The most the expanded view ever asks for, so a huge display cannot demand
    /// frames larger than the decoder pipe can carry.
    pub const MAX_EXPANDED_EDGE: i32 = 3840;
    const MAX_EXPANDED_PIXELS: i64 = 3840 * 2400;

    pub fn new(width: i32, height: i32) -> Self {
        Self {
            width: width.clamp(16, Self::MAX_EDGE),
            height: height.clamp(16, Self::MAX_EDGE),
            expanded: false,
            max_fps: 30,
        }
    }

    pub fn expanded(width: i32, height: i32) -> Self {
        let (mut width, mut height) = (
            width.clamp(16, Self::MAX_EXPANDED_EDGE),
            height.clamp(16, Self::MAX_EXPANDED_EDGE),
        );
        let pixels = i64::from(width) * i64::from(height);
        if pixels > Self::MAX_EXPANDED_PIXELS {
            let shrink = (Self::MAX_EXPANDED_PIXELS as f64 / pixels as f64).sqrt();
            width = ((f64::from(width) * shrink) as i32).max(16);
            height = ((f64::from(height) * shrink) as i32).max(16);
        }
        Self {
            width,
            height,
            expanded: true,
            max_fps: 60,
        }
    }

    /// Never larger than the screen the view is on.
    pub fn within(self, width: i32, height: i32) -> Self {
        Self {
            width: self.width.min(width.max(16)),
            height: self.height.min(height.max(16)),
            ..self
        }
    }

    /// The expanded view asks for 60 frames per second only on a screen that
    /// refreshes at least that fast.
    pub fn for_refresh_rate(self, millihertz: i32) -> Self {
        Self {
            max_fps: if self.expanded && millihertz >= 55_000 {
                60
            } else {
                30
            },
            ..self
        }
    }

    pub fn for_viewport(width: i32, height: i32, scale: i32) -> Self {
        Self::new(width.saturating_mul(scale), height.saturating_mul(scale))
    }

    pub fn for_expanded_viewport(width: i32, height: i32, scale: i32) -> Self {
        Self::expanded(width.saturating_mul(scale), height.saturating_mul(scale))
    }

    /// The same request with its limits applied again, for values that crossed a
    /// process boundary.
    pub fn normalized(self) -> Self {
        if self.expanded {
            Self {
                max_fps: if self.max_fps >= 60 { 60 } else { 30 },
                ..Self::expanded(self.width, self.height)
            }
        } else {
            Self::new(self.width, self.height)
        }
    }
}

/// Redacts diagnostic output; does not scrub plaintext from memory.
#[derive(Clone, Eq, PartialEq)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(secret: String) -> Self {
        Self(secret)
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

#[derive(Clone, Debug)]
pub struct PreviewRequest {
    pub id: PreviewRequestId,
    pub entry: FileEntry,
    pub text_byte_limit: usize,
    pub render_document: bool,
    pub pdf_page: i32,
    pub media_size: MediaPreviewSize,
    pub detail: PreviewDetail,
    pub model_palette: super::ModelPalette,
    pub archive_password: Option<SecretString>,
}

/// A decode request, not a playable file. Only the sandbox may open `path`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SandboxedMedia {
    pub(crate) path: PathBuf,
    pub(crate) size: MediaPreviewSize,
    pub(crate) backend: crate::sandbox::MediaPreviewBackend,
    pub(crate) input_owner: Option<PreviewInputLease>,
    /// Audio-name hint: skip attached pictures, but preserve any real video found by the probe.
    pub(crate) audio_only: bool,
}

impl SandboxedMedia {
    pub(crate) fn retain_input(mut self, owner: impl Send + Sync + 'static) -> Self {
        self.input_owner = Some(PreviewInputLease(std::sync::Arc::new(owner)));
        self
    }
}

/// Keeps a staged source alive across player clones, seeks, and worker teardown.
#[derive(Clone)]
pub(crate) struct PreviewInputLease(std::sync::Arc<dyn Send + Sync>);

impl std::fmt::Debug for PreviewInputLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PreviewInputLease")
    }
}

impl PartialEq for PreviewInputLease {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for PreviewInputLease {}

#[derive(Clone, Debug, PartialEq)]
pub enum PreviewContent {
    Text {
        content: String,
        truncated: bool,
    },
    Document {
        source: String,
        document: Option<DocumentLayout>,
        fallback_reason: Option<String>,
        warnings: Vec<String>,
        truncated: bool,
    },
    Rendered {
        document: DocumentLayout,
        warnings: Vec<String>,
    },
    Image,
    Media,
    Rasterized {
        png: Vec<u8>,
    },
    Model {
        png: Vec<u8>,
    },
    SandboxedMedia {
        media: SandboxedMedia,
    },
    Pdf {
        png: Vec<u8>,
        page: i32,
        pages: i32,
        text_layer: Option<Arc<PdfTextLayer>>,
    },
    Archive {
        tree: ArchivePreviewTree,
    },
    Unsupported,
}

/// Extracted text and per-character bounds for one rendered PDF page.
/// `glyphs[i]` locates the i-th char of `text` in rendered PNG pixels.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PdfTextLayer {
    pub width: f32,
    pub height: f32,
    pub text: String,
    pub glyphs: Vec<[f32; 4]>,
}

#[derive(Clone, Debug)]
pub struct Preview {
    pub request_id: PreviewRequestId,
    pub entry: FileEntry,
    pub content_type: String,
    pub content: PreviewContent,
}

pub(crate) const INCORRECT_ARCHIVE_PASSWORD: &str = "The password is incorrect.";

#[derive(Clone, Debug)]
pub enum PreviewEvent {
    Progress {
        request_id: PreviewRequestId,
        stage: ModelPreviewStage,
    },
    Ready(Preview),
    Failed {
        request_id: PreviewRequestId,
        entry: FileEntry,
        message: String,
    },
    NeedsPassword {
        request_id: PreviewRequestId,
        entry: FileEntry,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ModelPreviewStage {
    Reading,
    Thumbnail,
    Rendering { triangles: usize },
    Finishing,
}

impl ModelPreviewStage {
    pub fn label(self) -> String {
        match self {
            Self::Reading => "Reading model…".into(),
            Self::Thumbnail => "Reading thumbnail…".into(),
            Self::Finishing => "Finishing preview…".into(),
            Self::Rendering { triangles } => {
                let count = if triangles >= 1_000_000 {
                    format!("{:.1}M", triangles as f64 / 1_000_000.)
                } else if triangles >= 1_000 {
                    format!("{:.1}K", triangles as f64 / 1_000.)
                } else {
                    triangles.to_string()
                };
                format!("Rendering {count} triangles…")
            }
        }
    }
}

/// Speculative loads yield to interactive ones at every shared limit.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PreviewPriority {
    #[default]
    Current,
    Neighbor,
}

pub trait PreviewProvider {
    fn load(&self, request: PreviewRequest, emit: Rc<dyn Fn(PreviewEvent)>) -> LoadHandle;

    /// Starts work for an entry next to the selection. Dropping the handle
    /// need not cancel work that has already begun.
    fn preload(&self, request: PreviewRequest, emit: Rc<dyn Fn(PreviewEvent)>) -> LoadHandle {
        self.load(request, emit)
    }
}

fn content_type_for_path(path: &Path) -> glib::GString {
    gio::content_type_guess(Some(path), None::<&[u8]>).0
}

pub(crate) fn is_image_path(path: &Path) -> bool {
    content_type_for_path(path).starts_with("image/")
}

pub(crate) fn is_media_path(path: &Path) -> bool {
    let content_type = content_type_for_path(path);
    content_type.starts_with("audio/") || content_type.starts_with("video/")
}

pub(crate) fn supports_remote_video(name: &OsStr) -> bool {
    Path::new(name)
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "mov" | "mp4"))
}

/// Content worth rendering ahead of a selection. Text, documents and archives
/// are cheap enough to load on demand; models and covers hold the heavy permit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PreloadKind {
    Image,
    Pdf,
    Media,
}

pub(crate) fn preload_kind(name: &OsStr) -> Option<PreloadKind> {
    if is_model(name) || crate::sandbox::CoverFormat::for_name(name).is_some() {
        return None;
    }
    match content_family(&content_type_for_path(Path::new(name))) {
        PreviewContent::Pdf { .. } => Some(PreloadKind::Pdf),
        PreviewContent::Image => Some(PreloadKind::Image),
        PreviewContent::Media => Some(PreloadKind::Media),
        _ => None,
    }
}

pub(crate) fn is_model(name: &OsStr) -> bool {
    super::ModelFormat::for_name(name).is_some()
}

pub(crate) fn has_plain_text_extension(name: &OsStr) -> bool {
    Path::new(name)
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "conf" | "ini"))
}

pub(crate) fn normalize_preview_text(text: &str) -> Cow<'_, str> {
    if text.contains('\0') {
        Cow::Owned(text.replace('\0', "�"))
    } else {
        Cow::Borrowed(text)
    }
}

pub(crate) fn is_extensionless_dotfile(name: &OsStr) -> bool {
    let bytes = name.as_encoded_bytes();
    bytes.len() > 1 && bytes.starts_with(b".") && Path::new(name).extension().is_none()
}

pub(crate) fn is_non_executable_extensionless_dotfile(
    name: &OsStr,
    unix_mode: Option<u32>,
) -> bool {
    is_extensionless_dotfile(name) && unix_mode.is_some_and(|mode| mode & 0o111 == 0)
}

pub(crate) fn content_family(content_type: &str) -> PreviewContent {
    if content_type == "application/pdf" {
        PreviewContent::Pdf {
            png: Vec::new(),
            page: 0,
            pages: 0,
            text_layer: None,
        }
    } else if content_type == "image/gif" {
        PreviewContent::Media
    } else if content_type.starts_with("image/") {
        PreviewContent::Image
    } else if content_type.starts_with("audio/") || content_type.starts_with("video/") {
        PreviewContent::Media
    } else if content_type.starts_with("text/")
        || matches!(
            content_type,
            "application/json"
                | "application/ld+json"
                | "application/toml"
                | "application/yaml"
                | "application/x-yaml"
                | "application/xml"
                | "application/javascript"
                | "application/x-javascript"
                | "application/x-shellscript"
        )
        || content_type.ends_with("+json")
        || content_type.ends_with("+xml")
    {
        PreviewContent::Text {
            content: String::new(),
            truncated: false,
        }
    } else {
        PreviewContent::Unsupported
    }
}

/// Names are archive data, never paths to resolve against the host filesystem.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArchiveFileEntry {
    pub name: String,
    pub directory: bool,
    pub size: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArchiveDirectory {
    pub name: String,
    pub children: Vec<ArchiveNode>,
}

impl Drop for ArchiveDirectory {
    // Recursive drop glue can overflow the stack on attacker-controlled nesting.
    fn drop(&mut self) {
        if self.children.is_empty() {
            return;
        }
        let mut stack = vec![std::mem::take(&mut self.children)];
        while let Some(mut children) = stack.pop() {
            for child in children.drain(..) {
                if let ArchiveNode::Directory(mut directory) = child {
                    stack.push(std::mem::take(&mut directory.children));
                }
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ArchiveNode {
    Directory(ArchiveDirectory),
    File { name: String, size: u64 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArchivePreviewTree {
    pub root: ArchiveDirectory,
    pub file_count: usize,
}

/// Keep dots and backslashes literal; collapse empty slash components for virtual navigation.
/// The listing budget must use the same split to bound synthesized directory nodes.
pub(crate) fn split_archive_name(name: &str) -> Vec<&str> {
    name.split('/')
        .filter(|segment| !segment.is_empty())
        .collect()
}

/// Callers must enforce the listing name budget before constructing this unbounded tree.
pub fn archive_preview_tree(entries: Vec<ArchiveFileEntry>) -> ArchivePreviewTree {
    #[derive(Default)]
    struct Builder {
        directory: bool,
        name: String,
        size: u64,
        children: Vec<Builder>,
        // Index only directories: duplicate files must remain distinct members.
        dirs: std::collections::HashMap<String, usize>,
    }

    fn insert(seed: &mut Builder, segments: Vec<&str>, directory: bool, size: u64) {
        let Some(last) = segments.as_slice().split_last() else {
            return;
        };
        let mut current = seed;
        for parent in last.1 {
            let next = match current.dirs.get(*parent).copied() {
                Some(index) => &mut current.children[index],
                None => {
                    current
                        .dirs
                        .insert((*parent).to_owned(), current.children.len());
                    current.children.push(Builder {
                        directory: true,
                        name: (*parent).to_owned(),
                        ..Builder::default()
                    });
                    let last = current.children.len() - 1;
                    &mut current.children[last]
                }
            };
            current = next;
        }
        if directory {
            if !current.dirs.contains_key(*last.0) {
                current
                    .dirs
                    .insert((*last.0).to_owned(), current.children.len());
                current.children.push(Builder {
                    directory: true,
                    name: (*last.0).to_owned(),
                    ..Builder::default()
                });
            }
        } else {
            current.children.push(Builder {
                directory: false,
                name: (*last.0).to_owned(),
                size,
                ..Builder::default()
            });
        }
    }

    fn sort_and_convert(children: Vec<Builder>) -> (Vec<ArchiveNode>, usize) {
        // Use heap frames so hostile nesting cannot exhaust the call stack.
        struct Frame {
            name: String,
            children: Vec<Builder>,
            next: usize,
            nodes: Vec<ArchiveNode>,
            file_count: usize,
        }

        fn sort_level(children: &mut [Builder]) {
            children.sort_by_cached_key(|child| {
                (
                    std::cmp::Reverse(child.directory),
                    child.name.to_ascii_lowercase(),
                    child.name.clone(),
                )
            });
        }

        let mut root_children = children;
        sort_level(&mut root_children);
        let mut stack = vec![Frame {
            name: String::new(),
            children: root_children,
            next: 0,
            nodes: Vec::new(),
            file_count: 0,
        }];
        while !stack.is_empty() {
            let advanced = {
                let Some(frame) = stack.last_mut() else {
                    break;
                };
                if frame.next >= frame.children.len() {
                    None
                } else {
                    let child = std::mem::take(&mut frame.children[frame.next]);
                    frame.next += 1;
                    Some(child)
                }
            };
            let Some(child) = advanced else {
                let frame = stack
                    .pop()
                    .expect("levels complete before the stack empties");
                match stack.last_mut() {
                    Some(parent) => {
                        parent.file_count += frame.file_count;
                        parent.nodes.push(ArchiveNode::Directory(ArchiveDirectory {
                            name: frame.name,
                            children: frame.nodes,
                        }));
                    }
                    None => return (frame.nodes, frame.file_count),
                }
                continue;
            };
            if child.directory {
                let mut grandchildren = child.children;
                sort_level(&mut grandchildren);
                stack.push(Frame {
                    name: child.name,
                    children: grandchildren,
                    next: 0,
                    nodes: Vec::new(),
                    file_count: 0,
                });
            } else {
                let Some(frame) = stack.last_mut() else {
                    unreachable!("a file always has a level awaiting it");
                };
                frame.file_count += 1;
                frame.nodes.push(ArchiveNode::File {
                    name: child.name,
                    size: child.size,
                });
            }
        }
        unreachable!("archive levels complete before the stack empties");
    }

    let mut root = Builder {
        directory: true,
        ..Builder::default()
    };
    for entry in entries {
        let segments = split_archive_name(&entry.name);
        insert(&mut root, segments, entry.directory, entry.size);
    }
    let (children, file_count) = sort_and_convert(root.children);
    ArchivePreviewTree {
        root: ArchiveDirectory {
            name: String::new(),
            children,
        },
        file_count,
    }
}

pub(crate) fn archive_preview_format(name: &OsStr) -> Option<ArchiveFormat> {
    match ArchiveFormat::from_extension(&name.to_string_lossy()) {
        Some(
            format @ (ArchiveFormat::Zip
            | ArchiveFormat::SevenZ
            | ArchiveFormat::TarGz
            | ArchiveFormat::Tar),
        ) => Some(format),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
