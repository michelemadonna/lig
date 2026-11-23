//! Asynchronous file previews. Only the session adapter knows about completion state.
mod archive;
pub(crate) mod config;
mod content;
mod external;
pub(crate) mod git;
pub(crate) mod graphics;
pub(crate) mod layout;
pub(crate) mod locale;
mod media;
pub(crate) mod mouse;
mod native;
pub(crate) mod raster;
pub(crate) mod render;
pub(crate) mod target;
mod webm;
mod webp;

use std::collections::VecDeque;
use std::fs::Metadata;
use std::io::{Read, Seek};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use crossbeam_channel::{Receiver, Sender};

pub use config::Config;
use config::{Backend, Fallback, Output};
use native::{Content, Previewer};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub(crate) path: PathBuf,
    pub(crate) cwd: PathBuf,
    pub(crate) search_path: Option<String>,
    pub(crate) viewport: (u16, u16),
    pub(crate) light: bool,
    pub(crate) date_locale: Option<String>,
    pub(crate) show_hidden: bool,
    pub(crate) git: Option<target::Git>,
    pub(crate) git_width: Option<u16>,
}

#[derive(Debug)]
pub struct Document {
    pub(crate) content: Content,
    text_width: u16,
    pub(crate) git_diff: Option<git::Diff>,
}

impl Document {
    fn new(mut content: Content) -> Self {
        let text_width = match &mut content {
            Content::Text(text) => {
                text.trim_blank_tail();
                render::text_width(text)
            }
            _ => 0,
        };
        Self { content, text_width, git_diff: None }
    }

    pub(crate) fn image_size(&self) -> Option<(u32, u32)> {
        match &self.content {
            Content::Image(image) => Some(image.dimensions),
            _ => None,
        }
    }

    pub(crate) fn width(&self, ui: &crate::config::Ui) -> u16 {
        match &self.content {
            Content::Directory(rows) => render::tree_width(ui, rows),
            _ => self.text_width,
        }
    }

    pub(crate) fn bitmap(&self) -> Option<&str> {
        match &self.content {
            Content::Image(image) => image.payload.as_deref(),
            _ => None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Key {
    path: PathBuf,
    modified: SystemTime,
    len: u64,
    viewport: Option<(u16, u16)>,
    cwd: PathBuf,
    search_path: Option<String>,
    light: bool,
    date_locale: Option<String>,
    show_hidden: bool,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}

impl Key {
    fn new(request: &Request, meta: &Metadata, external: bool) -> Option<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        Some(Self {
            path: request.path.clone(),
            modified: meta.modified().ok()?,
            len: meta.len(),
            viewport: external.then_some(request.viewport),
            cwd: request.cwd.clone(),
            search_path: request.search_path.clone().filter(|_| external),
            light: request.light,
            date_locale: request.date_locale.clone(),
            show_hidden: request.show_hidden,
            #[cfg(unix)]
            identity: (meta.dev(), meta.ino(), meta.ctime(), meta.ctime_nsec()),
        })
    }
}

const CACHE_BUDGET: usize = 16 * 1024 * 1024;

struct CacheEntry {
    key: Key,
    document: Arc<Document>,
    weight: usize,
    expires: Option<Instant>,
}

struct Cache {
    items: VecDeque<CacheEntry>,
    bytes: usize,
    limit: usize,
}

impl Cache {
    fn new(limit: usize) -> Self {
        Self { items: VecDeque::new(), bytes: 0, limit }
    }

    fn get(&mut self, key: &Key) -> Option<Arc<Document>> {
        let index = self.items.iter().position(|entry| &entry.key == key)?;
        let entry = self.items.remove(index).unwrap();
        if entry.expires.is_some_and(|expires| expires <= Instant::now()) {
            self.bytes -= entry.weight;
            return None;
        }
        let document = entry.document.clone();
        self.items.push_front(entry);
        Some(document)
    }

    fn insert(&mut self, key: Key, document: Arc<Document>, expires: Option<Instant>) {
        let weight = document.content.weight() + 512;
        if self.limit == 0 || weight > CACHE_BUDGET {
            return;
        }
        while self.items.len() >= self.limit || self.bytes + weight > CACHE_BUDGET {
            self.bytes -= self.items.pop_back().unwrap().weight;
        }
        self.items.push_front(CacheEntry { key, document, weight, expires });
        self.bytes += weight;
    }
}

type Job = (u64, Request);

pub struct Worker {
    jobs: Sender<Job>,
    discard: Receiver<Job>,
    generation: Arc<AtomicU64>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    pub fn start(config: Config, ready: impl Fn(u64, Arc<Document>) + Send + 'static) -> std::io::Result<Self> {
        let (jobs, rx) = crossbeam_channel::bounded::<Job>(1);
        let discard = rx.clone();
        let generation = Arc::new(AtomicU64::new(0));
        let current = generation.clone();
        let thread = std::thread::Builder::new().name("lig-preview".into()).spawn(move || {
            let mut cache = Cache::new(config.cache_entries);
            while let Ok((id, request)) = rx.recv() {
                let cancelled = || current.load(Ordering::Acquire) != id;
                if cancelled() {
                    continue;
                }
                let document = load(&request, &config, &mut cache, &cancelled);
                if !cancelled() {
                    ready(id, document);
                }
            }
        })?;
        Ok(Self { jobs, discard, generation, thread: Some(thread) })
    }

    pub fn request(&self, request: Request) -> u64 {
        let id = self.cancel();
        // Only the latest job waits; the currently running one observes the generation change.
        let _ = self.jobs.try_send((id, request));
        id
    }

    pub fn cancel(&self) -> u64 {
        let id = self.generation.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
        let _ = self.discard.try_recv();
        id
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel();
        let (closed, _) = crossbeam_channel::bounded(0);
        drop(std::mem::replace(&mut self.jobs, closed));
        // Give an external child time to be reaped. A stuck filesystem must not hold up shell exit.
        let deadline = Instant::now() + Duration::from_millis(100);
        if let Some(thread) = self.thread.take() {
            while !thread.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(2));
            }
            if thread.is_finished() {
                let _ = thread.join();
            }
        }
    }
}

fn load(request: &Request, config: &Config, cache: &mut Cache, cancelled: &dyn Fn() -> bool) -> Arc<Document> {
    let path = &request.path;
    let plain = |text| Arc::new(Document::new(Content::Text(content::Text::plain(text, config.max_lines))));

    let ext = native::extension(path);
    if let Some(mode) = request.git
        && native::engine(config, &ext, native::Category::Text).backend == Backend::Native
        && let Some((text, format)) = git::preview(mode, request, config, cancelled)
    {
        let mut doc = Document::new(Content::Text(text));
        doc.git_diff = format;
        return Arc::new(doc);
    }

    let Ok(meta) = path.metadata() else {
        return plain("Preview unavailable");
    };
    if !(meta.is_file() || meta.is_dir()) {
        return Arc::new(Document::new(native::metadata(
            &meta,
            "Preview unavailable for this filesystem object",
            config.max_lines,
            request.date_locale.as_deref(),
        )));
    }
    let external_key = Key::new(request, &meta, true);
    let native_key = Key::new(request, &meta, false);
    if let Some(cached) = [&external_key, &native_key].into_iter().flatten().find_map(|key| cache.get(key)) {
        return cached;
    }

    let ext = if meta.is_dir() { String::new() } else { ext };
    let mut file = meta.is_file().then(|| native::open_file(path).ok()).flatten();
    let mut header = Vec::with_capacity(512);
    if let Some(file) = &mut file {
        let _ = file.take(512).read_to_end(&mut header);
        let _ = file.rewind();
    }
    let category = native::category(&ext, &header, meta.is_dir());
    let engine = native::engine(config, &ext, category);
    let is_external = engine.backend != Backend::Native;
    let cache_key = if is_external { external_key } else { native_key };
    if cancelled() {
        return plain("");
    }
    if is_external {
        let args = external::Arguments {
            file: path,
            cwd: &request.cwd,
            search_path: request.search_path.as_deref(),
            width: request.viewport.0,
            height: request.viewport.1,
            extension: &ext,
            max_lines: config.max_lines,
        };
        let result = external::command(engine, &args)
            .and_then(|command| {
                external::run(command, None, config.max_bytes, Duration::from_millis(config.timeout_ms), cancelled)
            })
            .and_then(|bytes| {
                match engine.output {
                    Output::Plain | Output::Ansi => {
                        content::external(&bytes, config.max_lines, engine.output == Output::Ansi)
                    }
                    Output::MediainfoJson => media::from_mediainfo(&bytes, meta.len(), config.max_lines),
                    Output::SevenZipList => archive::from_seven_zip(&bytes, config.max_lines),
                }
                .ok_or_else(|| anyhow::anyhow!("no usable preview output"))
            });
        match result {
            Ok(text) => {
                let result = Arc::new(Document::new(Content::Text(text)));
                if !cancelled()
                    && let Some(key) = cache_key
                {
                    cache.insert(key, result.clone(), None);
                }
                return result;
            }
            Err(err) if !cancelled() => crate::log(&format!("preview backend failed: {err}")),
            Err(_) => return plain(""),
        }
    }

    let content = if is_external && engine.fallback == Fallback::Metadata {
        native::metadata(&meta, "External preview unavailable", config.max_lines, request.date_locale.as_deref())
    } else {
        let opts = native::Options { config, light: request.light, show_hidden: request.show_hidden, cancelled };
        match category.preview(path, file, &header, &opts) {
            Ok(content) => content,
            Err(err) => {
                if !cancelled() {
                    crate::log(&format!("native preview unavailable: {err}"));
                }
                native::metadata(&meta, "Content unavailable", config.max_lines, request.date_locale.as_deref())
            }
        }
    };
    let result = Arc::new(Document::new(content));
    if !cancelled()
        && let Some(key) = cache_key
    {
        // Failure fallback is temporary so a newly available external tool can recover.
        let expires = if is_external {
            Some(Instant::now() + Duration::from_secs(5))
        } else if meta.is_dir() {
            // Changes to grandchildren may leave the root directory stamp unchanged.
            Some(Instant::now() + Duration::from_secs(2))
        } else {
            None
        };
        cache.insert(key, result.clone(), expires);
    }
    result
}

#[cfg(test)]
mod tests;
