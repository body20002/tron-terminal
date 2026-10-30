//! Project tasks. Run with `cargo xtask <task>`.
//!
//! `cargo xtask bench` measures tron the same way every time, so numbers from
//! different days and branches compare:
//!
//! - `e2e`: `cat` of large files inside tron, Alacritty and Ghostty at 100x30.
//! - `startup`: launch until the first command in the terminal runs.
//! - `headless`: parser and terminal throughput, without a window.
//! - `cells`: snapshot, cell build and upload of one frame (criterion).
//!
//! Generated files live in `target/bench/`. Every `e2e` and `startup` result is
//! also appended to `target/bench/history.csv`.

use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{BufWriter, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Parser, Subcommand, ValueEnum};

/// Grid size every terminal is measured at.
const COLUMNS: u16 = 100;
const ROWS: u16 = 30;
/// Longest a single run may take before it counts as hung.
const TIMEOUT: Duration = Duration::from_secs(120);
/// Attempts per run when the terminal ends at another grid size. Ghostty 1.4
/// sometimes starts the command one column and row short and resizes later.
const ATTEMPTS: usize = 3;
/// Pause between runs, so the compositor and the GPU settle.
const SETTLE: Duration = Duration::from_millis(300);

#[derive(Parser)]
#[command(about = "Project tasks")]
enum Task {
    /// Measure tron's speed
    #[command(subcommand)]
    Bench(Bench),
}

#[derive(Subcommand)]
enum Bench {
    /// `cat` of large files in each terminal, from the first byte to the last
    E2e {
        #[command(flatten)]
        terminals: Terminals,
        /// Workloads to run
        #[arg(long, value_delimiter = ',', default_value = "plain,sgr,unicode")]
        cases: Vec<Case>,
    },
    /// Launch until a command inside the terminal runs
    Startup {
        #[command(flatten)]
        terminals: Terminals,
    },
    /// Parser and terminal throughput without a window: [plain|sgr|unicode] [repeat]
    Headless {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Snapshot, cell build and upload per frame; arguments go to criterion
    Cells {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

#[derive(Args)]
struct Terminals {
    /// Runs per terminal and workload. tron's timings are bimodal; fewer than 9
    /// runs can show gains that are not there.
    #[arg(long, default_value_t = 9)]
    runs: usize,
    /// Terminals to measure
    #[arg(long, value_delimiter = ',', default_value = "tron,alacritty,ghostty")]
    terminals: Vec<Terminal>,
    /// tron binary to measure instead of a fresh release build, for example a
    /// copy of the build before a change
    #[arg(long, value_name = "PATH")]
    tron: Option<PathBuf>,
    /// Run every terminal on these CPUs with `taskset`, for example `0-7`. Which
    /// cores the scheduler picks for the shell, the reader and the renderer
    /// splits timings into two clusters; physical cores without their SMT
    /// siblings give one steady cluster for comparing builds.
    #[arg(long, value_name = "LIST")]
    cpus: Option<String>,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum Terminal {
    Tron,
    Alacritty,
    Ghostty,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum Case {
    /// Plain ASCII lines, 54 MiB
    Plain,
    /// True color SGR lines, 44 MiB
    Sgr,
    /// Mixed Unicode and CJK lines, 31 MiB
    Unicode,
}

impl Case {
    fn name(self) -> &'static str {
        match self {
            Case::Plain => "plain",
            Case::Sgr => "sgr",
            Case::Unicode => "unicode",
        }
    }

    fn size(self) -> usize {
        match self {
            Case::Plain => 54,
            Case::Sgr => 44,
            Case::Unicode => 31,
        }
    }

    fn line(self, i: usize, out: &mut String) {
        out.clear();
        let _ = match self {
            Case::Plain => writeln!(out, "line {i}: the quick brown fox jumps over the lazy dog"),
            Case::Sgr => writeln!(
                out,
                "\x1b[1;38;2;{};{};200m{i:>8}\x1b[0m \x1b[48;2;20;{};60mstatus\x1b[0m ok",
                i % 256,
                (i / 3) % 256,
                (i / 7) % 256
            ),
            Case::Unicode => writeln!(out, "{i} → λ 中文字符 ✓ ünïcödé 日本語のテキスト"),
        };
    }

    /// Writes the workload file once; its content never changes.
    fn file(self, dir: &Path) -> Result<PathBuf> {
        let path = dir.join(format!("{}.txt", self.name()));
        let size = self.size() << 20;
        if fs::metadata(&path).is_ok_and(|m| m.len() as usize >= size) {
            return Ok(path);
        }
        let mut out = BufWriter::new(File::create(&path)?);
        let (mut written, mut line) = (0, String::new());
        for i in 0.. {
            if written >= size {
                break;
            }
            self.line(i, &mut line);
            out.write_all(line.as_bytes())?;
            written += line.len();
        }
        out.flush()?;
        Ok(path)
    }
}

fn main() -> Result<()> {
    match Task::parse() {
        Task::Bench(Bench::E2e { terminals, cases }) => e2e(&terminals, &cases),
        Task::Bench(Bench::Startup { terminals }) => startup(&terminals),
        Task::Bench(Bench::Headless { args }) => {
            cargo(&["run", "--release", "-p", "tron-core", "--example", "throughput", "--"], &args)
        }
        Task::Bench(Bench::Cells { args }) => {
            cargo(&["bench", "-p", "tron-render", "--features", "bench", "--bench", "cells", "--"], &args)
        }
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("xtask sits in the workspace").to_owned()
}

fn bench_dir() -> Result<PathBuf> {
    let target = std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| root().join("target"), PathBuf::from);
    let dir = target.join("bench");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn cargo(args: &[&str], extra: &[String]) -> Result<()> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo).args(args).args(extra).current_dir(root()).status()?;
    ensure!(status.success(), "cargo {} failed", args.join(" "));
    Ok(())
}

/// The terminals to run, each with the command that starts it.
struct Launcher {
    dir: PathBuf,
    tron: PathBuf,
    cpus: Option<String>,
}

impl Launcher {
    fn new(terminals: &Terminals) -> Result<Self> {
        let dir = bench_dir()?;
        let tron = match &terminals.tron {
            Some(path) => path.canonicalize().with_context(|| format!("no tron binary at {}", path.display()))?,
            None if terminals.terminals.iter().any(|t| matches!(t, Terminal::Tron)) => {
                cargo(&["build", "--release", "-p", "tron"], &[])?;
                std::env::var_os("CARGO_TARGET_DIR")
                    .map_or_else(|| root().join("target"), PathBuf::from)
                    .join("release/tron")
            }
            None => PathBuf::new(),
        };
        // Every terminal runs with its default settings, not the user's.
        let config = dir.join("tron");
        fs::create_dir_all(&config)?;
        fs::write(
            config.join("config.toml"),
            format!("[window]\ncolumns = {COLUMNS}\nrows = {ROWS}\n\n[updates]\ncheck = false\n"),
        )?;
        fs::write(dir.join("alacritty.toml"), "")?;
        Ok(Self { dir, tron, cpus: terminals.cpus.clone() })
    }

    /// Starts `terminal` running `sh -c script sh args...`.
    fn command(&self, terminal: Terminal, script: &str, args: &[&Path]) -> Command {
        let mut command = match terminal {
            Terminal::Tron => {
                let mut command = Command::new(&self.tron);
                command.arg("--config-dir").arg(self.dir.join("tron")).arg("-e");
                command
            }
            Terminal::Alacritty => {
                let mut command = Command::new("alacritty");
                command
                    .arg("--config-file")
                    .arg(self.dir.join("alacritty.toml"))
                    .arg("-o")
                    .arg(format!("window.dimensions.columns={COLUMNS}"))
                    .arg("-o")
                    .arg(format!("window.dimensions.lines={ROWS}"))
                    .arg("-e");
                command
            }
            Terminal::Ghostty => {
                let mut command = Command::new("ghostty");
                // A single instance would hand the window to a running Ghostty and exit.
                command.args([
                    "--config-default-files=false",
                    "--gtk-single-instance=false",
                    &format!("--window-width={COLUMNS}"),
                    &format!("--window-height={ROWS}"),
                    "-e",
                ]);
                command
            }
        };
        command.args(["sh", "-c", script, "sh"]).args(args);
        if let Some(cpus) = &self.cpus {
            // `taskset` replaces itself with the terminal, so killing it kills the terminal.
            let mut pinned = Command::new("taskset");
            pinned.arg("-c").arg(cpus).arg(command.get_program()).args(command.get_args());
            command = pinned;
        }
        command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        command
    }

    /// Runs `terminal` until the script writes its result file, then kills it.
    /// The script writes `$(stty size)` and its value on one line. With
    /// `check_size`, a run that ended at another grid size is repeated; startup
    /// needs no check, its command runs before a terminal may resize. Returns
    /// when the terminal was launched and the value.
    fn run(&self, terminal: Terminal, script: &str, args: &[&Path], check_size: bool) -> Result<(SystemTime, String)> {
        let mut attempt = 1;
        loop {
            let (launched, (rows, cols), value) = self.run_once(terminal, script, args)?;
            if !check_size || (rows, cols) == (ROWS, COLUMNS) {
                return Ok((launched, value));
            }
            ensure!(attempt < ATTEMPTS, "{terminal:?} keeps ending at {cols}x{rows} instead of {COLUMNS}x{ROWS}");
            eprintln!("{terminal:?} ended at {cols}x{rows} instead of {COLUMNS}x{ROWS}, repeating the run");
            attempt += 1;
        }
    }

    fn run_once(&self, terminal: Terminal, script: &str, args: &[&Path]) -> Result<(SystemTime, (u16, u16), String)> {
        let result = self.dir.join("result");
        let _ = fs::remove_file(&result);
        let mut args = args.to_vec();
        args.push(&result);
        let launched = SystemTime::now();
        let mut child =
            self.command(terminal, script, &args).spawn().with_context(|| format!("failed to start {terminal:?}"))?;
        let deadline = Instant::now() + TIMEOUT;
        let text = loop {
            // `echo` writes the line at once; a partial file has no newline yet.
            if let Ok(text) = fs::read_to_string(&result)
                && text.ends_with('\n')
            {
                break Ok(text.trim().to_owned());
            }
            if let Some(status) = child.try_wait()? {
                break Err(anyhow::anyhow!("{terminal:?} exited ({status}) before the command finished"));
            }
            if Instant::now() > deadline {
                break Err(anyhow::anyhow!("{terminal:?} did not finish within {TIMEOUT:?}"));
            }
            thread::sleep(Duration::from_millis(5));
        };
        // Killed rather than closed: a terminal that is slow to exit, or crashes
        // on exit, should not count against the next run.
        let _ = child.kill();
        let _ = child.wait();
        thread::sleep(SETTLE);
        let text = text?;
        let fields: Vec<&str> = text.split_whitespace().collect();
        let [rows, cols, value] = fields[..] else { bail!("unexpected result {text:?}") };
        let size = (rows.parse()?, cols.parse()?);
        Ok((launched, size, value.to_owned()))
    }

    fn version(&self, terminal: Terminal) -> String {
        let output = match terminal {
            Terminal::Tron => Command::new(&self.tron).arg("--version").output(),
            Terminal::Alacritty => Command::new("alacritty").arg("--version").output(),
            Terminal::Ghostty => Command::new("ghostty").arg("+version").output(),
        };
        output
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| s.lines().find(|l| !l.trim().is_empty()).map(|l| l.trim().to_owned()))
            .unwrap_or_else(|| "unknown version".into())
    }
}

fn e2e(terminals: &Terminals, cases: &[Case]) -> Result<()> {
    let launcher = Launcher::new(terminals)?;
    let files = cases.iter().map(|case| case.file(&launcher.dir)).collect::<Result<Vec<_>>>()?;
    // Time from before the first byte to after the last, measured inside the
    // terminal, in microseconds.
    let script = r#"s=$(date +%s%N); cat "$1"; e=$(date +%s%N); echo "$(stty size) $(((e - s) / 1000))" > "$2""#;
    print_header("e2e", &launcher, terminals);
    for (case, file) in cases.iter().zip(&files) {
        for &terminal in &terminals.terminals {
            let mut samples = Vec::with_capacity(terminals.runs);
            for _ in 0..terminals.runs {
                let (_, text) = launcher.run(terminal, script, &[file], true)?;
                let micros: f64 = text.parse().with_context(|| format!("unexpected result {text:?}"))?;
                samples.push(micros / 1000.0);
            }
            report("e2e", case.name(), terminal, &mut samples)?;
        }
    }
    Ok(())
}

fn startup(terminals: &Terminals) -> Result<()> {
    let launcher = Launcher::new(terminals)?;
    let script = r#"t=$(date +%s%N); echo "$(stty size) $t" > "$1""#;
    print_header("startup", &launcher, terminals);
    for &terminal in &terminals.terminals {
        let mut samples = Vec::with_capacity(terminals.runs);
        for _ in 0..terminals.runs {
            let (launched, text) = launcher.run(terminal, script, &[], false)?;
            let ran: u128 = text.parse().with_context(|| format!("unexpected result {text:?}"))?;
            let launched = launched.duration_since(UNIX_EPOCH)?.as_nanos();
            samples.push(ran.saturating_sub(launched) as f64 / 1e6);
        }
        report("startup", "launch", terminal, &mut samples)?;
    }
    Ok(())
}

fn print_header(kind: &str, launcher: &Launcher, terminals: &Terminals) {
    let pinned = terminals.cpus.as_ref().map(|cpus| format!(", on CPUs {cpus}")).unwrap_or_default();
    println!("{kind} at {COLUMNS}x{ROWS}, {} runs{pinned}, milliseconds, lower is better", terminals.runs);
    for &terminal in &terminals.terminals {
        println!("  {terminal:?}: {}", launcher.version(terminal));
    }
    println!("{:<9} {:<10} {:>8} {:>8} {:>8}  runs, sorted", "workload", "terminal", "median", "min", "max");
}

/// Prints one result line and appends it to `history.csv`. Every run is shown,
/// sorted, so two clusters of timings stand out.
fn report(kind: &str, case: &str, terminal: Terminal, samples: &mut [f64]) -> Result<()> {
    if samples.is_empty() {
        bail!("no runs");
    }
    samples.sort_by(f64::total_cmp);
    let median = match samples.len() {
        n if n % 2 == 1 => samples[n / 2],
        n => (samples[n / 2 - 1] + samples[n / 2]) / 2.0,
    };
    let runs = samples.iter().map(|s| format!("{s:.0}")).collect::<Vec<_>>().join(" ");
    let terminal = format!("{terminal:?}").to_lowercase();
    println!("{case:<9} {terminal:<10} {median:>8.1} {:>8.1} {:>8.1}  {runs}", samples[0], samples[samples.len() - 1]);

    let history = bench_dir()?.join("history.csv");
    let new = !history.exists();
    let mut file = fs::OpenOptions::new().create(true).append(true).open(&history)?;
    if new {
        writeln!(file, "unix_time,revision,kind,workload,terminal,median_ms,runs_ms")?;
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    writeln!(file, "{now},{},{kind},{case},{terminal},{median:.1},{runs}", revision())?;
    Ok(())
}

/// The checked out commit, marked when the tree has changes.
fn revision() -> String {
    let git = |args: &[&str]| Command::new("git").args(args).current_dir(root()).output().ok().map(|o| o.stdout);
    let hash = git(&["rev-parse", "--short", "HEAD"])
        .and_then(|out| String::from_utf8(out).ok())
        .map_or_else(|| "unknown".into(), |s| s.trim().to_owned());
    let dirty = git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|out| !out.is_empty());
    if dirty { format!("{hash}-dirty") } else { hash }
}
