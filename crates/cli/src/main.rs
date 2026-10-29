//! `mediaanon` — command-line front-end of MediaAnonymizer.

mod args;
mod batch;

use anyhow::{Context, Result};
use args::{Cli, Command, ProcessArgs};
use clap::Parser;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use mediaanon_core::{self as core, CancelToken, MediaKind, Processor, Report};
use rayon::prelude::*;
use serde::Serialize;
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_logging(&cli);
    let result = match &cli.command {
        Command::Process(p) => process(&cli, p),
        Command::Inspect { files } => inspect(&cli, files),
        Command::Devices => devices(&cli),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            if cli.json {
                println!("{}", serde_json::json!({ "error": format!("{e:#}") }));
            } else {
                eprintln!("error: {e:#}");
            }
            ExitCode::FAILURE
        }
    }
}

fn init_logging(cli: &Cli) {
    let level = match (cli.quiet, cli.verbose) {
        (true, _) => "error",
        (_, 0) => "warn",
        (_, 1) => "info",
        _ => "debug",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_env("MEDIAANON_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(format!("{level},ort=warn")));
    tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr).with_target(false).init();
}

/// Machine-readable outcome of one file.
#[derive(Serialize)]
#[serde(untagged)]
enum JsonLine<'a> {
    Ok(&'a Report),
    Skipped { input: &'a Path, skipped: &'a str },
    Err { input: &'a Path, error: String },
}

fn process(cli: &Cli, args: &ProcessArgs) -> Result<ExitCode> {
    let options = args.options();
    options.validate().map_err(|_| {
        anyhow::anyhow!(
            "nothing to do: pass --strip-metadata, --strip-sensitive, --faces and/or --timestamp (see --help)"
        )
    })?;

    let edits = match &args.video.timeline {
        Some(path) => {
            let text = std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
            let e: core::Edits =
                serde_json::from_str(&text).with_context(|| format!("invalid timeline {}", path.display()))?;
            e.validate().with_context(|| format!("invalid timeline {}", path.display()))?;
            e
        }
        None => core::Edits::default(),
    };

    let dest = batch::Destination::new(args.output.as_deref(), &args.suffix, args.in_place, &args.inputs)?;
    let jobs = batch::plan(&args.inputs, args.recursive, &dest)?;
    if jobs.is_empty() {
        anyhow::bail!("no supported photo or video found in the given inputs");
    }

    let mut skipped = 0;
    let jobs: Vec<batch::Job> = jobs
        .into_iter()
        .filter(|j| {
            let exists = j.output.exists() && j.output != j.input;
            if exists && !args.overwrite {
                skipped += 1;
                report_skip(cli, &j.input, "output exists (use --overwrite)");
            }
            !exists || args.overwrite
        })
        .collect();

    if args.dry_run {
        for j in &jobs {
            println!("{} -> {}", j.input.display(), j.output.display());
        }
        return Ok(ExitCode::SUCCESS);
    }

    let processor = Processor::new(options).context("cannot initialise the processor")?;
    if let (Some(dev), false) = (processor.inference_device(), cli.quiet || cli.json) {
        eprintln!("face detection on {dev}");
    }

    let cancel = CancelToken::new();
    {
        let c = cancel.clone();
        ctrlc::set_handler(move || {
            if c.is_cancelled() {
                std::process::exit(130);
            }
            eprintln!("\ncancelling (press Ctrl-C again to abort immediately)...");
            c.cancel();
        })
        .ok();
    }

    for j in &jobs {
        if let Some(parent) = j.output.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
        }
    }

    // Photos are processed in parallel; each video already uses every core.
    let (videos, photos): (Vec<_>, Vec<_>) =
        jobs.iter().partition(|j| matches!(core::media::detect(&j.input), Ok(MediaKind::Video)));

    let ui = Ui::new(cli, jobs.len());
    let failed = AtomicUsize::new(0);
    let run = |job: &batch::Job, frames_bar: Option<&ProgressBar>| {
        let on_progress = |p: core::Progress| {
            if let Some(bar) = frames_bar {
                if let Some(t) = p.total {
                    bar.set_length(t);
                }
                bar.set_position(p.done);
            }
        };
        let result = processor.process_file_with(&job.input, &job.output, &edits, &on_progress, &cancel);
        if result.is_err() {
            failed.fetch_add(1, Ordering::Relaxed);
        }
        ui.finish_file(&job.input, result);
    };

    let pool = rayon::ThreadPoolBuilder::new().num_threads(args.jobs.unwrap_or(0)).build()?;
    pool.install(|| photos.par_iter().for_each(|j| run(j, None)));
    for job in videos {
        if cancel.is_cancelled() {
            break;
        }
        let bar = ui.video_bar(&job.input);
        run(job, bar.as_ref());
        if let Some(b) = bar {
            b.finish_and_clear();
        }
    }
    ui.done();

    let failed = failed.into_inner();
    if !cli.quiet && !cli.json {
        let ok = jobs.len() - failed;
        eprintln!("{ok} processed, {failed} failed, {skipped} skipped");
    }
    Ok(if failed > 0 || cancel.is_cancelled() { ExitCode::FAILURE } else { ExitCode::SUCCESS })
}

fn report_skip(cli: &Cli, input: &Path, why: &str) {
    if cli.json {
        println!("{}", serde_json::to_string(&JsonLine::Skipped { input, skipped: why }).unwrap());
    } else if !cli.quiet {
        eprintln!("skipped {}: {why}", input.display());
    }
}

/// Progress bars and per-file output.
struct Ui {
    multi: Option<MultiProgress>,
    overall: Option<ProgressBar>,
    json: bool,
    verbose: bool,
    quiet: bool,
    lock: Mutex<()>,
}

impl Ui {
    fn new(cli: &Cli, files: usize) -> Self {
        let interactive = !cli.quiet && !cli.json && std::io::IsTerminal::is_terminal(&std::io::stderr());
        let multi = interactive.then(MultiProgress::new);
        let overall = multi.as_ref().map(|m| {
            let bar = m.add(ProgressBar::new(files as u64));
            bar.set_style(
                ProgressStyle::with_template("{spinner} [{elapsed_precise}] {bar:30} {pos}/{len} files").unwrap(),
            );
            bar.enable_steady_tick(std::time::Duration::from_millis(120));
            bar
        });
        Self { multi, overall, json: cli.json, verbose: cli.verbose > 0, quiet: cli.quiet, lock: Mutex::new(()) }
    }

    fn video_bar(&self, input: &Path) -> Option<ProgressBar> {
        let m = self.multi.as_ref()?;
        let bar = m.add(ProgressBar::new(0));
        bar.set_style(ProgressStyle::with_template("  {msg} {bar:30} {percent:>3}% {per_sec} eta {eta}").unwrap());
        bar.set_message(file_name(input));
        Some(bar)
    }

    fn println(&self, line: String) {
        match &self.multi {
            Some(m) => {
                let _ = m.println(line);
            }
            None => eprintln!("{line}"),
        }
    }

    fn finish_file(&self, input: &Path, result: core::Result<Report>) {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(b) = &self.overall {
            b.inc(1);
        }
        match result {
            Ok(r) if self.json => println!("{}", serde_json::to_string(&JsonLine::Ok(&r)).unwrap()),
            Err(e) if self.json => {
                println!("{}", serde_json::to_string(&JsonLine::Err { input, error: e.to_string() }).unwrap())
            }
            Ok(r) if !self.quiet => {
                self.println(summary(&r));
                for w in &r.warnings {
                    self.println(format!("    ! {w}"));
                }
                if self.verbose {
                    for n in &r.notes {
                        self.println(format!("    · {n}"));
                    }
                    for m in &r.removed {
                        self.println(format!("    - {m}"));
                    }
                }
            }
            Ok(_) => {}
            Err(e) => self.println(format!("✗ {}: {e}", input.display())),
        }
    }

    fn done(&self) {
        if let Some(b) = &self.overall {
            b.finish_and_clear();
        }
    }
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn summary(r: &Report) -> String {
    let mut parts = Vec::new();
    match (r.frames, r.frames_with_faces) {
        (Some(frames), Some(with)) => parts.push(format!("{with}/{frames} frames with faces")),
        _ if r.faces > 0 => parts.push(format!("{} face{}", r.faces, if r.faces == 1 { "" } else { "s" })),
        _ => {}
    }
    if let Some(t) = &r.timestamp {
        parts.push(format!("stamped {t}"));
    }
    if !r.removed.is_empty() {
        parts.push(format!("{} metadata item{} removed", r.removed.len(), if r.removed.len() == 1 { "" } else { "s" }));
    }
    parts.push(format!("{:.1}s", r.elapsed.as_secs_f64()));
    format!("✓ {} → {}  ({})", r.input.display(), r.output.display(), parts.join(" · "))
}

fn inspect(cli: &Cli, files: &[std::path::PathBuf]) -> Result<ExitCode> {
    let mut failed = false;
    for f in files {
        match core::inspect(f) {
            Ok(findings) if cli.json => {
                println!("{}", serde_json::json!({ "input": f, "metadata": findings }));
            }
            Ok(findings) => {
                println!("{}", f.display());
                if findings.is_empty() {
                    println!("  no metadata found");
                }
                for x in findings {
                    let mark = if x.sensitive { "!" } else { " " };
                    println!("  {mark} {:<28} {}", x.block, x.detail);
                }
            }
            Err(e) => {
                failed = true;
                eprintln!("{}: {e}", f.display());
            }
        }
    }
    if !cli.json && !cli.quiet {
        println!("\n  ! = removed by --strip-sensitive (everything listed is removed by --strip-metadata,\n      except blocks marked as needed for rendering)");
    }
    Ok(if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS })
}

fn devices(cli: &Cli) -> Result<ExitCode> {
    let inference = core::available_devices();
    let video = core::video::capabilities();
    let interesting = |e: &&String| {
        ["264", "265", "hevc", "av1", "vp9", "vp8", "ffv1", "prores", "mjpeg"].iter().any(|k| e.contains(k))
    };
    if cli.json {
        let inf = inference.as_ref().map_err(|e| e.to_string());
        let vid = video.map_err(|e| e.to_string());
        println!(
            "{}",
            serde_json::json!({
                "inference": inf.as_ref().ok(),
                "inference_error": inf.as_ref().err(),
                "ffmpeg": vid.as_ref().ok(),
                "ffmpeg_error": vid.as_ref().err(),
            })
        );
        return Ok(ExitCode::SUCCESS);
    }
    println!("Face detection (ONNX Runtime):");
    match &inference {
        Ok(list) => {
            for d in list {
                let state = if d.available {
                    "available".to_string()
                } else if d.feature.is_empty() {
                    "unavailable".to_string()
                } else {
                    format!("unavailable (build with --features {} and a matching ONNX Runtime)", d.feature)
                };
                println!("  {:<10} {state}", d.device.to_string());
            }
        }
        Err(e) => println!("  error: {e}"),
    }
    println!("\nVideo (FFmpeg):");
    match video {
        Ok(c) => {
            println!("  version    {}", c.version);
            println!("  hwaccel    {}", if c.hwaccels.is_empty() { "none".into() } else { c.hwaccels.join(", ") });
            let enc: Vec<&String> = c.encoders.iter().filter(interesting).collect();
            for chunk in enc.chunks(6) {
                let names: Vec<&str> = chunk.iter().map(|s| s.as_str()).collect();
                println!("  encoders   {}", names.join(", "));
            }
            println!("\n  Listed back-ends are compiled into FFmpeg; they also need a matching GPU and driver.");
        }
        Err(e) => println!("  error: {e}"),
    }
    Ok(ExitCode::SUCCESS)
}
