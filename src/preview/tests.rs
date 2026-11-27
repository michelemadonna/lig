use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::SystemTime;

use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};

use super::*;
use crate::engine::{Kind, Suggestion};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let index = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("lig-preview-test-{}-{index}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn file(&self, name: &str, body: impl AsRef<[u8]>) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, body).unwrap();
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn request(path: PathBuf) -> Request {
    Request {
        cwd: path.parent().unwrap().to_owned(),
        path,
        search_path: None,
        viewport: (32, 10),
        light: false,
        date_locale: None,
        show_hidden: false,
        git: None,
        git_width: None,
    }
}

fn read(request: &Request, config: &Config) -> Arc<Document> {
    load(request, config, &mut Cache::new(config.cache_entries), &|| false)
}

fn text(doc: &Document) -> String {
    let Content::Text(doc) = &doc.content else { panic!("expected text") };
    doc.lines
        .iter()
        .map(|line| line.iter().map(|span| span.text.as_str()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn configuration_is_opt_in_validated_and_resolves_rules() {
    let parse = crate::config::Config::parse;
    assert!(!parse("").unwrap().preview.enabled);
    let cfg = parse(
        r#"
        [preview]
        enabled = true
        show_on = "on_hover"
        layout = "floating"
        width = "50%"
        max_width = "80%"
        on_hover_open_delay = 450
        animation = "reveal"
        animation_duration_ms = 600
        screen_margin = 8
        [preview.text]
        max_lines = 120
        line_numbers = false
        syntax_highlighting = false
        backend = "shell"
        script = "printf '%s' \"$LIG_PREVIEW_FILE\""
        [preview.image]
        image_protocol = "half_blocks"
        max_lines = 80
        [preview.rules.".JSON"]
        backend = "command"
        command = ["jq", ".", "{file}"]
    "#,
    )
    .unwrap()
    .preview;
    assert_eq!(cfg.layout, config::Layout::Floating);
    assert_eq!(cfg.show_on, config::ShowOn::OnHover);
    assert_eq!(cfg.on_hover_open_delay, 450);
    assert_eq!(cfg.animation, config::Animation::Reveal);
    assert_eq!(cfg.animation_duration_ms, 600);
    assert_eq!(cfg.screen_margin, 8);
    assert_eq!(cfg.text.max_lines, 120);
    assert_eq!(cfg.text.line_numbers, Some(false));
    assert_eq!(cfg.text.syntax_highlighting, Some(false));
    assert_eq!(cfg.image.image_protocol, Some(config::ImageProtocol::HalfBlocks));
    assert_eq!(cfg.image.max_lines, 80);
    assert_eq!(cfg.width.resolve(120), Some(60));
    assert_eq!(native::engine(&cfg, "json", native::Category::Text).backend, Backend::Command);
    assert_eq!(native::engine(&cfg, "yaml", native::Category::Text).backend, Backend::Shell);
    assert_eq!(native::engine(&cfg, "zip", native::Category::Archive).backend, Backend::Native);
    for table in [
        "[preview]\nwidth = '200%'",
        "[preview]\nheight = 'typo'",
        "[preview]\nwidth = 'auto'",
        "[preview]\nheight = 'auto'",
        "[preview]\nmax_bytes = 0",
        "[preview]\nmax_lines = 2",
        "[preview]\nposition = 'auto'",
        "[preview]\nstart_hidden = true",
        "[preview]\nauto_open_delay_ms = 10",
        "[preview]\nlayout = 'column'",
        "[preview]\non_hover_open_delay = 30001",
        "[preview.text]\nmax_lines = 5000",
        "[preview.image]\nline_numbers = false",
        "[preview.text]\nimage_protocol = 'cells'",
        "[preview]\nanimation_duration_ms = 30001",
        "[preview]\nscreen_margin = 41",
        "[preview]\nanimation = 'slide'",
        "[preview]\nunknown = 1",
        "[preview.text]\nbackend = 'command'",
        "[preview.text]\nbackend = 'shell'",
        "[preview.keys]\nup = 'not a key'",
    ] {
        assert!(parse(table).is_err(), "{table}");
    }
    let defaults = toml::to_string_pretty(&crate::config::Config::default()).unwrap();
    assert!(!parse(&defaults).unwrap().preview.enabled);
}

#[test]
fn filesystem_adapter_uses_insert_value_and_respects_the_typed_path_prefix() {
    let dir = Fixture::new();
    let filenames = ["sp ace.rs", "é&x.rs", "a=b", "a's.rs"];
    let item = |insert: String, delete| Suggestion {
        label: "decorative display label".into(),
        description: String::new(),
        kind: Kind::File,
        icon: None,
        insert,
        delete,
        back: 0,
        query: String::new(),
    };
    for file in filenames {
        let s = item(format!("{} ", crate::parser::escape(file)), 1);
        assert_eq!(target::file_from_candidate("cat x", &s, &dir.0), Some(dir.0.join(file)));
    }
    let sep = std::path::MAIN_SEPARATOR;
    let prefix = format!("cat sub{sep}x");
    assert_eq!(
        target::file_from_candidate(&prefix, &item("ordinary.rs ".into(), 1), &dir.0),
        Some(dir.0.join("sub").join("ordinary.rs"))
    );
    assert!(target::file_from_candidate("cat x", &item("one.bin | another.bin ".into(), 1), &dir.0).is_none());
    assert_eq!(target::git_context("git diff --cached fil", "git", &Default::default()), Some(target::Git::Staged));
    assert_eq!(
        target::git_context("git diff -- --cached fil", "git", &Default::default()),
        Some(target::Git::Worktree)
    );
}

#[test]
fn category_detection_uses_names_and_magic() {
    use native::Category::*;
    assert_eq!(native::extension(Path::new("data.TAR.GZ")), "tar.gz");
    assert_eq!(native::category("", b"GIF89a", false), Image);
    assert_eq!(native::category("png", b"bad bytes", false), Image);
    assert_eq!(native::category("", b"PK\x03\x04", false), Archive);
    assert_eq!(native::category("tar.gz", b"", false), Archive);
    assert_eq!(native::category("ini", b"[section]", false), Text);
    assert_eq!(native::category("png", b"", true), Directory);
}

#[test]
fn native_text_preserves_source_and_applies_theme_and_bounds() {
    let dir = Fixture::new();
    let src = "pub fn demo() -> bool { true }\n// second\n// third";
    let source = request(dir.file("file.rs", src));
    let mut config = Config { ..Config::default() };
    config.text.max_lines = 2;
    let dark = read(&source, &config);
    let Content::Text(doc) = &dark.content else { panic!("text expected") };
    assert_eq!(text(&dark), "pub fn demo() -> bool { true }\n// second");
    assert!(doc.numbered);
    assert!(doc.lines[0].iter().any(|span| span.style.fg.is_some()));
    let mut light_request = source.clone();
    light_request.light = true;
    let light = read(&light_request, &config);
    let Content::Text(light_doc) = &light.content else { panic!("text expected") };
    assert_ne!(light_doc.lines[0][0].style.fg, doc.lines[0][0].style.fg);
    config.max_bytes = 6;
    config.text.line_numbers = Some(false);
    config.text.syntax_highlighting = Some(false);
    let limited = read(&source, &config);
    assert_eq!(text(&limited), "pub fn");
    let Content::Text(doc) = &limited.content else { panic!("text expected") };
    assert!(!doc.numbered && doc.lines[0][0].style == content::Style::default());

    let raw = dir.file("bad.txt", b"partial\xffutf8");
    assert_eq!(text(&read(&request(raw), &Config::default())), "partial�utf8");
    let binary = dir.file("binary", b"this\x00is\x00binary");
    assert!(text(&read(&request(binary), &Config::default())).contains("Content unavailable"));
}

#[test]
fn required_filetypes_have_color_grammars() {
    let syntaxes = two_face::syntax::extra_newlines();
    for ext in
        ["md", "json", "toml", "yaml", "ini", "xml", "html", "css", "sh", "py", "rs", "java", "js", "ts", "c", "cpp"]
    {
        assert!(syntaxes.find_syntax_by_extension(ext).is_some(), "missing grammar: {ext}");
    }
}

#[test]
fn image_formats_and_first_gif_frame_are_decoded_natively() {
    let dir = Fixture::new();
    let image = DynamicImage::ImageRgba8(RgbaImage::from_pixel(30, 10, Rgba([220, 12, 30, 255])));
    for (name, format) in [
        ("pic.png", ImageFormat::Png),
        ("pic.jpg", ImageFormat::Jpeg),
        ("pic.jpeg", ImageFormat::Jpeg),
        ("UPPER.JPEG", ImageFormat::Jpeg),
        ("renamed.txt", ImageFormat::Jpeg),
        ("pic.gif", ImageFormat::Gif),
        ("pic.webp", ImageFormat::WebP),
        ("pic.bmp", ImageFormat::Bmp),
    ] {
        let path = dir.0.join(name);
        if format == ImageFormat::Gif {
            let mut encoder = image::codecs::gif::GifEncoder::new(File::create(&path).unwrap());
            encoder.encode_frame(image::Frame::new(image.to_rgba8())).unwrap();
            encoder.encode_frame(image::Frame::new(RgbaImage::from_pixel(30, 10, Rgba([30, 255, 12, 255])))).unwrap();
        } else {
            image.save_with_format(&path, format).unwrap();
        }
        let result = read(&request(path), &Config::default());
        let Content::Image(img) = &result.content else { panic!("{name}: {}", text(&result)) };
        assert_eq!(img.dimensions, (30, 10));
        assert!(img.pixels.get_pixel(15, 5)[0] > 170, "{name}");
    }
    let corrupt = dir.file("broken.png", b"\x89PNG\r\n\x1a\n");
    assert!(text(&read(&request(corrupt), &Config::default())).contains("Content unavailable"));
    // A valid, highly compressible image beyond the pixel cap is rejected before raster allocation.
    let huge = dir.0.join("oversized.png");
    RgbaImage::new(3000, 3000).save(&huge).unwrap();
    assert!(text(&read(&request(huge), &Config::default())).contains("Content unavailable"));
}

#[test]
fn archives_list_names_and_sizes_without_extracting() {
    let dir = Fixture::new();
    let zip = dir.0.join("sample.zip");
    let mut zip_writer = zip::ZipWriter::new(File::create(&zip).unwrap());
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip_writer.add_directory("nested/", options).unwrap();
    zip_writer.start_file("nested/entry.txt", options).unwrap();
    zip_writer.write_all(b"four").unwrap();
    zip_writer.finish().unwrap();
    let contents = text(&read(&request(zip.clone()), &Config::default()));
    assert!(contents.contains("nested/entry.txt   4 B") && contents.contains("2 entries"), "{contents}");
    let limited = Config { archive: config::Engine { max_lines: 2, ..Config::default().archive }, ..Config::default() };
    assert!(text(&read(&request(zip), &limited)).contains("1 of 2 entries"));

    let mut writer = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(4);
    header.set_mode(0o644);
    writer.append_data(&mut header, "nested/item.txt", &b"four"[..]).unwrap();
    let tar = writer.into_inner().unwrap();
    let tar_path = dir.file("archive.tar", &tar);
    assert!(text(&read(&request(tar_path), &Config::default())).contains("nested/item.txt   4 B"));
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gzip.write_all(&tar).unwrap();
    let gzip = gzip.finish().unwrap();
    let archive = dir.file("archive.tar.gz", &gzip);
    assert!(text(&read(&request(archive), &Config::default())).contains("TAR.GZ · 1 entries"));
    for name in ["document.gz", "renamed.zip"] {
        let archive = dir.file(name, &gzip);
        assert!(text(&read(&request(archive), &Config::default())).contains("Last member size"));
    }
    assert!(!dir.0.join("nested").exists());
}

#[test]
fn archive_limits_handle_corruption_and_large_tar_streams() {
    let dir = Fixture::new();
    let zip = dir.file("corrupt.zip", b"PK\x03\x04small");
    assert!(text(&read(&request(zip), &Config::default())).contains("Content unavailable"));

    let mut writer = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
    let mut header = tar::Header::new_gnu();
    header.set_size(16 * 1024 * 1024);
    header.set_mode(0o644);
    writer.append_data(&mut header, "large", std::io::repeat(0).take(16 * 1024 * 1024)).unwrap();
    let gzip = writer.into_inner().unwrap().finish().unwrap();
    let archive = dir.file("archive.tar.gz", gzip);
    let output = text(&read(&request(archive), &Config::default()));
    assert!(output.contains("large") && output.contains("partial"), "{output}");
}

#[test]
fn cache_reuses_content_but_checks_file_stamp_and_settings() {
    let dir = Fixture::new();
    let path = dir.file("file.txt", "old contents");
    let request = request(path.clone());
    let mut cache = Cache::new(1);
    let config = Config::default();
    let first = load(&request, &config, &mut cache, &|| false);
    assert!(Arc::ptr_eq(&first, &load(&request, &config, &mut cache, &|| false)));
    fs::write(&path, "new contents").unwrap();
    File::options().write(true).open(&path).unwrap().set_modified(SystemTime::UNIX_EPOCH).unwrap();
    let current = load(&request, &config, &mut cache, &|| false);
    assert_eq!(text(&current), "new contents");
    assert!(!Arc::ptr_eq(&first, &current));
    let mut moved = request.clone();
    moved.viewport = (44, 16);
    assert!(Arc::ptr_eq(&current, &load(&moved, &config, &mut cache, &|| false)));
    moved.light = true;
    assert!(!Arc::ptr_eq(&current, &load(&moved, &config, &mut cache, &|| false)));
    assert_eq!(cache.items.len(), 1);
}

#[cfg(unix)]
#[test]
fn external_argv_and_shell_keep_unicode_and_metacharacters_as_data() {
    let dir = Fixture::new();
    let request = request(dir.file("é '\" ; $({width}).txt", "native fallback"));
    let cfg = crate::config::Config::parse(
        r#"
        [preview]
        [preview.text]
        max_lines = 12
        backend = "command"
        command = ["printf", "%s\n", "file={file}", "{width}x{height}/{max_lines}:{extension}"]
        output = "plain"
    "#,
    )
    .unwrap()
    .preview;
    let rendered = text(&read(&request, &cfg));
    assert_eq!(rendered, format!("file={}\n32x10/12:txt", request.path.display()));
    let cfg = crate::config::Config::parse(r#"
        [preview.text]
        backend = "shell"
        interpreter = "sh"
        script = 'printf "%s:%sx%s:%s" "$LIG_PREVIEW_FILE" "$LIG_PREVIEW_WIDTH" "$LIG_PREVIEW_HEIGHT" "$LIG_PREVIEW_EXT"'
    "#).unwrap().preview;
    assert_eq!(text(&read(&request, &cfg)), format!("{}:32x10:txt", request.path.display()));
}

#[cfg(unix)]
#[test]
fn external_failure_timeout_and_overflow_fall_back_to_native() {
    let dir = Fixture::new();
    let request = request(dir.file("file.txt", "native fallback"));
    for command in [
        vec!["lig-test-missing-preview-command"],
        vec!["sh", "-c", "exit 3"],
        vec!["sh", "-c", "sleep 4"],
        vec!["printf", "%0220d", "0"],
        vec!["printf", "\x1b[31m"],
        vec!["sh", "-c", "printf '%0400d' 0 >&2"],
    ] {
        let mut cfg = Config { max_bytes: 64, timeout_ms: 90, ..Config::default() };
        cfg.text.backend = Backend::Command;
        cfg.text.command = command.iter().map(|s| (*s).to_owned()).collect();
        let start = Instant::now();
        assert_eq!(text(&read(&request, &cfg)), "native fallback", "{command:?}");
        assert!(start.elapsed() < Duration::from_secs(2));
        cfg.text.fallback = Fallback::Metadata;
        assert!(text(&read(&request, &cfg)).contains("External preview unavailable"), "{command:?}");
    }
}

#[cfg(unix)]
#[test]
fn obsolete_work_is_cancelled_and_only_the_latest_document_is_delivered() {
    let dir = Fixture::new();
    let first = request(dir.file("old.txt", "old"));
    let latest = request(dir.file("new.rs", "latest marker"));
    let started = dir.0.join("started");
    let leaked = dir.0.join("leaked");
    let mut config = Config { timeout_ms: 5000, ..Config::default() };
    config.rules.insert(
        "txt".into(),
        config::Engine {
            backend: Backend::Shell,
            script: Some("touch started; (sleep .8; touch leaked) & wait; printf obsolete".into()),
            ..Default::default()
        },
    );
    let (tx, rx) = crossbeam_channel::unbounded();
    let worker = Worker::start(config, move |id, doc| {
        tx.send((id, doc)).unwrap();
    })
    .unwrap();
    worker.request(first);
    let deadline = Instant::now() + Duration::from_secs(2);
    while !started.exists() {
        assert!(Instant::now() < deadline, "external preview did not start");
        std::thread::sleep(Duration::from_millis(10));
    }
    let expected = worker.request(latest);
    let (id, doc) = rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(id, expected);
    assert_eq!(text(&doc), "latest marker");
    assert!(rx.recv_timeout(Duration::from_millis(850)).is_err());
    assert!(!leaked.exists(), "an obsolete preview child survived");
}

#[cfg(windows)]
#[test]
fn windows_job_reaps_a_descendant_when_the_leader_exits() {
    let dir = Fixture::new();
    let source = request(dir.file("preview.txt", "native fallback"));
    let powershell =
        PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let descendant =
        "Start-Sleep -Milliseconds 500; [IO.File]::WriteAllText((Join-Path (Get-Location).Path 'leaked'), 'orphan')";
    let script: Vec<u8> = descendant.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let script = crate::remote::base64(&script);
    let mut config = Config { timeout_ms: 8000, ..Config::default() };
    config.text.backend = Backend::Shell;
    config.text.interpreter = Some(powershell.to_string_lossy().into_owned());
    config.text.script = Some(format!(
        "$ErrorActionPreference = 'Stop'; Start-Process -FilePath (Join-Path $PSHOME 'powershell.exe') -WorkingDirectory (Get-Location).Path -ArgumentList '-NoProfile -EncodedCommand {script}'; [Console]::Write('parent result')"
    ));
    assert_eq!(text(&read(&source, &config)), "parent result");
    std::thread::sleep(Duration::from_secs(2));
    assert!(!dir.0.join("leaked").exists(), "the preview descendant escaped its job");
}

#[test]
fn zip64_central_directory_uses_64_bit_counts_and_offsets() {
    let dir = Fixture::new();
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    writer.start_file("inside.txt", options).unwrap();
    writer.write_all(b"data").unwrap();
    let mut zip = writer.finish().unwrap().into_inner();
    let offset = zip.len() - 22;
    let eocd = zip.split_off(offset);
    let as_offset = |bytes: &[u8]| u64::from(u32::from_le_bytes(bytes[..4].try_into().unwrap()));
    let mut record = b"PK\x06\x06".to_vec();
    record.extend_from_slice(&44u64.to_le_bytes());
    record.extend_from_slice(&[45, 0, 45, 0]);
    record.extend_from_slice(&[0; 8]);
    for n in [1, 1, as_offset(&eocd[12..]), as_offset(&eocd[16..])] {
        record.extend_from_slice(&n.to_le_bytes());
    }
    zip.extend_from_slice(&record);
    zip.extend_from_slice(b"PK\x06\x07\0\0\0\0");
    zip.extend_from_slice(&(offset as u64).to_le_bytes());
    zip.extend_from_slice(&1u32.to_le_bytes());
    zip.extend_from_slice(&eocd[..8]);
    zip.extend_from_slice(&[255; 12]);
    zip.extend_from_slice(&eocd[20..]);
    let path = dir.file("zip64.zip", zip);
    assert!(text(&read(&request(path), &Config::default())).contains("inside.txt   4 B"));
}

#[test]
fn contextual_git_uses_raw_index_and_worktree_content() {
    use std::process::Command;
    let dir = Fixture::new();
    let git = |argv: &[&str]| {
        Command::new("git")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", if cfg!(windows) { "NUL" } else { "/dev/null" })
            .current_dir(&dir.0)
            .args(argv)
            .output()
    };
    if git(&["--version"]).is_err() {
        return;
    }
    let run = |args| {
        let output = git(args).unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    };
    run(&["init", "--template="]);
    let path = dir.file("file with spaces.rs", "base\n");
    run(&["add", "--", "file with spaces.rs"]);
    run(&["-c", "user.name=Preview Test", "-c", "user.email=test@example.invalid", "commit", "-qm", "base"]);
    fs::write(&path, "staged\n").unwrap();
    run(&["add", "--", "file with spaces.rs"]);
    fs::write(&path, "working\n").unwrap();
    let mut preview = request(path);
    preview.git = Some(target::Git::Worktree);
    let output = text(&read(&preview, &Config::default()));
    assert!(output.contains("-staged") && output.contains("+working"), "{output}");
    preview.git = Some(target::Git::Staged);
    let output = text(&read(&preview, &Config::default()));
    assert!(output.contains("-base") && output.contains("+staged"), "{output}");
}

#[test]
fn preview_key_defaults_distinguish_visibility_layout_and_size() {
    use crate::keys::{self, Key};
    let config = Config::default();
    let toggle = Key::parse_name(&config.keys.toggle).unwrap();
    let layout = Key::parse_name(config.keys.toggle_layout.as_deref().unwrap()).unwrap();
    let resize = Key::parse_name(config.keys.resize_floating.as_deref().unwrap()).unwrap();
    assert_eq!(keys::split(b"\x1bf")[0].1, layout);
    assert_eq!(keys::split(b"\x1b[102;3u")[0].1, layout);
    assert_eq!(keys::split(b"\x1bz")[0].1, resize);
    assert_eq!(keys::split(b"\x1bp")[0].1, toggle);
    assert_ne!(layout, Key::ctrl('p'));

    let config = crate::config::Config::parse("[preview]\nshow_controls=true\npreserve_completion=true").unwrap();
    let roundtrip = toml::to_string(&config).unwrap();
    assert!(!roundtrip.contains("show_controls") && !roundtrip.contains("preserve_completion"));
    assert!(crate::config::Config::parse("[preview.keys]\ntoggle_layout='alt-P'\nopen='shift-alt-p'").is_err());
    let defaults = crate::config::Config::parse("").unwrap().preview;
    assert_eq!(defaults.show_on, crate::preview::config::ShowOn::Always);
    assert_eq!(defaults.on_hover_open_delay, 500);
    assert_eq!(defaults.animation, crate::preview::config::Animation::None);
    assert_eq!(defaults.animation_duration_ms, 300);
    assert_eq!(defaults.screen_margin, 10);
}

#[test]
fn preview_key_overrides_survive_new_defaults() {
    for (input, layout, resize) in [
        ("toggle='alt-p'", "alt-f", "alt-z"),
        ("toggle_layout='shift-alt-p'\nresize_floating='alt-f'", "shift-alt-p", "alt-f"),
        ("resize_floating='alt-f'", "", "alt-f"),
        ("open='alt-z'", "alt-f", ""),
        ("toggle_layout=''", "", "alt-z"),
    ] {
        let config = crate::config::Config::parse(&format!("[preview.keys]\n{input}")).unwrap();
        assert_eq!(config.preview.keys.toggle_layout.as_deref(), Some(layout), "{input}");
        assert_eq!(config.preview.keys.resize_floating.as_deref(), Some(resize), "{input}");
    }
}

#[test]
fn directory_tree_is_sorted_bounded_and_refreshes_nested_entries() {
    let dir = Fixture::new();
    fs::create_dir_all(dir.0.join("a-dir/child/deep")).unwrap();
    dir.file("a-dir/child/visible.rs", "");
    dir.file("a-dir/child/deep/excluded.txt", "");
    dir.file("z-file", "");
    dir.file(".hidden", "");
    #[cfg(unix)]
    {
        let outside = Fixture::new();
        outside.file("from-link", "");
        std::os::unix::fs::symlink(&outside.0, dir.0.join("link")).unwrap();
        let mut config = Config::default();
        let linked = read(&request(dir.0.clone()), &config);
        let Content::Directory(rows) = &linked.content else { panic!("expected tree") };
        assert!(rows.iter().any(|row| row.name == "link" && row.directory));
        assert!(!rows.iter().any(|row| row.name == "from-link"));

        config.directory.max_lines = 2;
        assert_eq!(read(&request(dir.0.clone()), &config).content.lines(), 2);
    }
    let mut request = request(dir.0.clone());
    let config = Config::default();
    let mut cache = Cache::new(2);
    let first = load(&request, &config, &mut cache, &|| false);
    let Content::Directory(rows) = &first.content else { panic!("expected tree") };
    assert_eq!(rows[0].name, "a-dir");
    assert_eq!(rows.last().unwrap().name, "z-file");
    assert!(rows.iter().any(|row| row.name == "visible.rs" && row.prefix.chars().count() == 12));
    assert!(!rows.iter().any(|row| row.name == "excluded.txt" || row.name == ".hidden"));
    dir.file("a-dir/child/new.rs", "");
    cache.items.front_mut().unwrap().expires = Some(Instant::now());
    let changed = load(&request, &config, &mut cache, &|| false);
    let Content::Directory(rows) = &changed.content else { panic!("expected tree") };
    assert!(rows.iter().any(|row| row.name == "new.rs"));
    request.show_hidden = true;
    let hidden = read(&request, &config);
    let Content::Directory(rows) = &hidden.content else { panic!("expected tree") };
    assert!(rows.iter().any(|row| row.name == ".hidden"));
}

#[test]
fn text_with_unicode_bom_is_decoded_before_binary_detection() {
    let dir = Fixture::new();
    let sample = "日本語\r\nsecond line";
    let utf8 = dir.file("bom.txt", [b"\xef\xbb\xbf".as_slice(), sample.as_bytes()].concat());
    assert_eq!(text(&read(&request(utf8), &Config::default())), "日本語\nsecond line");
    for (name, little_endian) in [("script.ps1", true), ("renamed.jpg", false)] {
        let mut bytes = if little_endian { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
        for word in sample.encode_utf16() {
            bytes.extend(if little_endian { word.to_le_bytes() } else { word.to_be_bytes() });
        }
        assert_eq!(text(&read(&request(dir.file(name, bytes)), &Config::default())), "日本語\nsecond line");
    }
}

#[test]
fn localized_timestamp_is_first_in_generic_metadata() {
    let dir = Fixture::new();
    let path = dir.file("binary.blob", [0, 0, 0, 1]);
    let timestamp = SystemTime::UNIX_EPOCH + Duration::from_secs(1_713_144_000);
    File::options().write(true).open(&path).unwrap().set_modified(timestamp).unwrap();
    let mut request = request(path);
    request.date_locale = Some("C".into());
    let config = Config { text: config::Engine { max_lines: 1, ..Config::default().text }, ..Config::default() };
    let expected = locale::modified(timestamp, Some("C")).unwrap();
    assert_eq!(text(&read(&request, &config)), format!("Modified: {expected}"));
    assert!(locale::modified(timestamp, Some("not_an_installed_locale")).is_some());

    let vars = vec![("LANG".into(), "en_US.UTF-8".into()), ("LC_TIME".into(), "it_IT.UTF-8".into())];
    assert_eq!(locale::date_locale(&vars).as_deref(), Some("it_IT.UTF-8"));
    let vars = [vars, vec![("LC_ALL".into(), "C".into())]].concat();
    assert_eq!(locale::date_locale(&vars).as_deref(), Some("C"));
}

#[test]
fn media_detection_preserves_source_and_image_previews() {
    use native::Category::{Image, Media, Text};

    let ts = include_bytes!("../../tests/fixtures/media/clip.ts");
    let media = include_bytes!("../../tests/fixtures/media/tail.mp4");
    let matroska = include_bytes!("../../tests/fixtures/media/tracks.mkv");

    assert_eq!(native::category("ts", b"let value: string = 'hello';", false), Text);
    assert_eq!(native::category("ts", &ts[..512], false), Media);
    assert_eq!(native::category("data", &media[..512], false), Media);
    assert_eq!(native::category("data", &matroska[..512], false), Media);
    assert_eq!(native::category("mp4", b"\0\0\0\x18ftypisom\0\0\0\0avifiso2", false), Image);
    assert_eq!(native::category("m3u8", b"#EXTM3U\n", false), Text);
}

#[test]
fn webm_is_recognized_and_defaults_to_file_metadata() {
    let webm = include_bytes!("../../tests/fixtures/media/clip.webm");
    assert_eq!(native::category("data", &webm[..512], false), native::Category::Media);
    let fixture = Fixture::new();
    let path = fixture.file("clip.data", webm);
    let summary = text(&read(&request(path), &Config::default()));
    assert!(summary.contains("Modified:"), "{summary}");
    assert!(summary.contains("Content unavailable"), "{summary}");
}

#[test]
fn mediainfo_json_formats_track_summaries_and_rejects_missing_tracks() {
    let bytes = br#"{"media":{"track":[
        {"@type":"General","Format":"Matroska","Duration":1.023,"OverallBitRate":"281971"},
        {"@type":"Video","Format":"AVC","Width":"160","Height":96,"FrameRate":"24.000"},
        {"@type":"Audio","Format":"AAC","Channels":"1","SamplingRate":"48000","Language":"en"},
        {"@type":"Audio","Format":"AAC","Channels":"2","Language":"it"},
        {"@type":"Text","Format":"UTF-8","Language":"it\n\u001b"}
    ]}}"#;
    let render = |max_lines| {
        let content = Content::Text(media::from_mediainfo(bytes, 36057, max_lines).unwrap());
        text(&Document::new(content))
    };
    assert_eq!(
        render(8),
        "Matroska · 35.2 KiB · 0:00:01.023 · 282.0 kb/s\nVideo 1 · AVC · 160 × 96 · 24 fps\nAudio 1 · AAC · 1 channel · 48000 Hz · en\nAudio 2 · AAC · 2 channels · it\nSubtitles 1 · UTF-8 · it��"
    );
    assert_eq!(render(2).lines().count(), 2);
    assert!(media::from_mediainfo(br#"{"media":{"track":[{"@type":"Image"}]}}"#, 10, 8).is_none());
    assert!(media::from_mediainfo(b"invalid JSON", 10, 8).is_none());
}

#[test]
fn seven_zip_list_formats_members_and_rejects_incomplete_records() {
    let bytes = "7-Zip\r\n--\r\nPath = input.7z\r\nType = 7z\r\n\r\n----------\r\n\
        Path = nested\r\nSize = 0\r\nAttributes = D drwxr-xr-x\r\n\r\n\
        Path = nested/漢字.txt\r\nSize = 4\r\nEncrypted = +\r\n";
    let render = |max_lines| {
        let content = Content::Text(archive::from_seven_zip(bytes.as_bytes(), max_lines).unwrap());
        text(&Document::new(content))
    };
    assert_eq!(render(8), "nested/\nnested/漢字.txt   4 B   [encrypted]\n7z · 2 entries · 1 directories");
    assert_eq!(render(2), "nested/\n7z · 1 of 2 entries · 1 directories");
    assert!(archive::from_seven_zip(b"Type = 7z\nEnter password:", 8).is_none());
    assert!(archive::from_seven_zip(b"Type = 7z\n----------\nPath = item\nmissing separator\n", 8).is_none());
}
