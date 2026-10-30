//! A pipeline that keeps rows between frames, moving them with scrolls and
//! rebuilding only what changed, must draw the same pixels as a new pipeline
//! that builds everything.
//!
//! Needs a GPU and the `bench` feature:
//! `cargo nextest run -p tron-render --features bench`. Without a GPU it passes
//! without checking anything.

use tron_core::{Parser, Snapshot, Terminal};
use tron_font::FontSystem;
use tron_render::bench::CellBench;

/// The system's monospace font, then fonts with ligatures where installed: the
/// cursor splits ligatures, so only they show a row the cursor left drawn stale.
const FAMILIES: [&str; 5] = ["monospace", "Iosevka", "JetBrains Mono", "Fira Code", "Cascadia Code"];

/// Terminal output, then the smooth scroll offset in pixels the frame is drawn at.
const STEPS: &[(&str, &[u8], f32)] = &[
    ("first lines", b"one\r\n\x1b[31mtwo\x1b[0m\r\nthree", 0.0),
    ("fill the screen", b"\r\n4\r\n5\r\n6\r\n7\r\n8", 0.0),
    ("scroll by one", b"\r\n\x1b[1;44mnine\x1b[0m", 0.0),
    ("scroll by three", b"\r\nten\r\neleven\r\ntwelve", 0.0),
    ("cursor moves up and output scrolls", b"\x1b[3;5Hx\x1b[8;1H\r\nthirteen", 0.0),
    ("hidden cursor", b"\x1b[?25l\r\nfourteen\r\nfifteen\x1b[?25h", 0.0),
    ("cursor scrolls off the top", b"\x1b[1;1H\x1b[8;1H\r\n1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n7", 0.0),
    ("ligatures", b"\r\na -> b == c != d\r\n<= >= =>", 0.0),
    ("cursor parked in a ligature", b"\x1b[7;4H", 0.0),
    ("scroll with the cursor away", b"\x1b[8;1H\r\nnext", 0.0),
    ("underlines", b"\r\n\x1b[4mplain\x1b[0m \x1b[4:3mcurly\x1b[0m \x1b[9mstruck\x1b[0m", 0.0),
    ("smooth scroll starts", b"", 5.0),
    ("smooth scroll moves", b"", 11.0),
    ("output during smooth scroll", b"\r\nmore\x1b[42m green \x1b[0m", 7.0),
    ("smooth scroll ends", b"", 0.0),
    ("wide text", "\r\n中文字符 ✓\r\nend".as_bytes(), 0.0),
    ("more than a screen", b"\r\na\r\nb\r\nc\r\nd\r\ne\r\nf\r\ng\r\nh\r\ni", 0.0),
    ("clear", b"\x1b[2J\x1b[Hfresh", 0.0),
];

#[test]
fn kept_rows_draw_the_same_pixels_as_rebuilt_ones() {
    let Ok(fonts) = FontSystem::new("monospace", 12.0, 1.0) else {
        eprintln!("skipped: no monospace font");
        return;
    };
    let (cols, rows) = (40, 8);
    let (Ok(mut kept), Ok(mut fresh)) = (CellBench::new(&fonts, cols, rows), CellBench::new(&fonts, cols, rows)) else {
        eprintln!("skipped: no GPU");
        return;
    };
    for family in FAMILIES {
        let Ok(mut fonts) = FontSystem::new(family, 12.0, 1.0) else {
            continue;
        };
        kept.reset(&fonts);
        let mut term = Terminal::new(cols, rows, 100);
        let mut parser = Parser::new();
        let mut snapshot = Snapshot::default();
        for &(step, input, scroll) in STEPS {
            parser.advance(&mut term, input);
            snapshot.want_overscan = scroll > 0.0;
            term.snapshot(&mut snapshot);
            kept.set_scroll_offset(scroll);
            kept.frame(&snapshot, &mut fonts);
            fresh.reset(&fonts);
            fresh.set_scroll_offset(scroll);
            fresh.frame(&snapshot, &mut fonts);
            let (a, b) = (kept.pixels(), fresh.pixels());
            assert!(a == b, "{family}: pixels differ after {step}");
        }
    }
}
