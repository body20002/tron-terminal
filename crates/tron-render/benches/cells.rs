//! Cost of turning terminal state into a GPU frame: snapshot, cell instance
//! build and upload, for the damage patterns real output causes.
//!
//! Run with `cargo xtask bench cells`, or
//! `cargo bench -p tron-render --features bench --bench cells`.
//! Needs a GPU; without one it prints why and measures nothing.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use tron_core::{Parser, Snapshot, Terminal};
use tron_font::FontSystem;
use tron_render::bench::CellBench;

/// Grid sizes: the end to end benchmark's window, and a large maximized one.
const SIZES: [(usize, usize); 2] = [(100, 30), (240, 70)];

/// A terminal with every row filled, so each scroll moves text.
struct Screen {
    term: Terminal,
    parser: Parser,
    snapshot: Snapshot,
    line: u64,
}

impl Screen {
    fn new(cols: usize, rows: usize) -> Self {
        let mut screen = Self {
            term: Terminal::new(cols, rows, 10_000),
            parser: Parser::new(),
            snapshot: Snapshot::default(),
            line: 0,
        };
        for _ in 0..rows {
            screen.write_line();
        }
        screen
    }

    /// Writes a new line at the bottom, scrolling the screen by one row. Lines
    /// differ, like `cat` output, so a built row is never reused by accident.
    fn write_line(&mut self) {
        self.line += 1;
        let line = format!("\r\n{:>8} the quick brown fox jumps over the lazy dog 0123456789", self.line);
        self.parser.advance(&mut self.term, line.as_bytes());
    }

    fn snapshot(&mut self) -> &Snapshot {
        self.term.snapshot(&mut self.snapshot);
        &self.snapshot
    }
}

fn cells(c: &mut Criterion) {
    let mut fonts = match FontSystem::new("monospace", 12.0, 1.0) {
        Ok(fonts) => fonts,
        Err(error) => {
            eprintln!("skipped: no monospace font: {error}");
            return;
        }
    };
    let mut group = c.benchmark_group("cells");
    for (cols, rows) in SIZES {
        let mut bench = match CellBench::new(&fonts, cols, rows) {
            Ok(bench) => bench,
            Err(error) => {
                eprintln!("skipped: no GPU: {error}");
                return;
            }
        };
        let size = format!("{cols}x{rows}");
        let mut screen = Screen::new(cols, rows);
        // The first frame builds every row and fills the glyph atlas.
        bench.frame(screen.snapshot(), &mut fonts);

        // `cat` at one line per frame: the whole screen moves up one row.
        group.bench_function(BenchmarkId::new("scroll_1", &size), |b| {
            b.iter(|| {
                screen.write_line();
                black_box(bench.frame(screen.snapshot(), &mut fonts))
            });
        });

        // A flood: half a screen of new lines between two frames.
        group.bench_function(BenchmarkId::new("scroll_half", &size), |b| {
            b.iter(|| {
                for _ in 0..rows / 2 {
                    screen.write_line();
                }
                black_box(bench.frame(screen.snapshot(), &mut fonts))
            });
        });

        // A status line or progress counter: one row changes in place.
        let mut tick = 0u32;
        group.bench_function(BenchmarkId::new("one_row", &size), |b| {
            b.iter(|| {
                tick = tick.wrapping_add(1);
                let update = format!("\x1b7\x1b[{};1H{tick:>10}\x1b8", rows / 2);
                screen.parser.advance(&mut screen.term, update.as_bytes());
                black_box(bench.frame(screen.snapshot(), &mut fonts))
            });
        });

        // Nothing changed: what a frame asked for by a timer or input costs.
        bench.frame(screen.snapshot(), &mut fonts);
        group.bench_function(BenchmarkId::new("idle", &size), |b| {
            b.iter(|| black_box(bench.frame(screen.snapshot(), &mut fonts)));
        });
    }
    group.finish();
}

criterion_group!(benches, cells);
criterion_main!(benches);
