//! Title crawls: rolling credits, tickers and the sci-fi crawl, drawn as an
//! export draws them (skipped without a GPU adapter).
use reelforge_core::{Clip, ClipId, CrawlKind, FrameRate, Project, Sequence, TextCrawl, Time, TitleSpec};
use reelforge_export::render_still;
use reelforge_render::headless_device;

const W: u32 = 320;
const H: u32 = 180;

fn project(kind: CrawlKind, text: &str, start_off: bool) -> Project {
    let mut p = Project::new();
    let mut seq = Sequence::new("Crawl", W, H, FrameRate::new(25, 1));
    let spec = TitleSpec { text: text.into(), size: 28.0, color: [255, 255, 255, 255], shadow: false, crawl: TextCrawl { kind, start_off, ..TextCrawl::default() }, ..TitleSpec::default() };
    seq.tracks[0].clips = vec![Clip::new_title(ClipId(1), Time::ZERO, Time::from_secs_f64(4.0), spec)];
    p.sequences = vec![seq];
    p
}

/// The lit pixels' count and middle (x, y).
fn lit(img: &[u8]) -> (usize, f32, f32) {
    let (mut n, mut sx, mut sy) = (0usize, 0.0f32, 0.0f32);
    for (i, p) in img.chunks_exact(4).enumerate() {
        if p[0] > 128 {
            n += 1;
            sx += (i as u32 % W) as f32;
            sy += (i as u32 / W) as f32;
        }
    }
    let k = n.max(1) as f32;
    (n, sx / k, sy / k)
}

#[test]
fn titles_roll_crawl_and_fly_off_into_the_distance() {
    let Some((d, q)) = headless_device() else { return };
    let at = |p: &Project, t: f64| render_still(p, 0, Time::from_secs_f64(t), &d, &q).unwrap().0;
    let credits = "DIRECTED BY\nSOMEONE\n\nMUSIC BY\nSOMEONE ELSE\n\nTHE END";

    // Rolling credits: below the frame at the start, rising through it, gone at the end.
    let roll = project(CrawlKind::RollUp, credits, true);
    assert_eq!(lit(&at(&roll, 0.0)).0, 0, "starts just below the frame");
    let (n1, _, y1) = lit(&at(&roll, 1.2));
    let (n2, _, y2) = lit(&at(&roll, 2.0));
    let (n3, _, y3) = lit(&at(&roll, 2.8));
    assert!(n1 > 50 && n2 > 50 && n3 > 50, "{n1} {n2} {n3}");
    assert!(y1 > y2 + 10.0 && y2 > y3 + 10.0, "rising: {y1} {y2} {y3}");
    assert_eq!(lit(&at(&roll, 3.99)).0, 0, "gone off the top at the end");
    // Lines that were off the canvas come into view (nothing is cut off): halfway
    // through, the frame is full of text from top to bottom.
    let mid = at(&roll, 2.0);
    let rows = |a: u32, b: u32| (a..b).any(|y| (0..W).any(|x| mid[((y * W + x) * 4) as usize] > 128));
    assert!(rows(0, 30) && rows(150, 180), "text at both edges halfway through");
    // Not starting off screen: the first line is in at once, at the top.
    let (n0, _, y0) = lit(&at(&project(CrawlKind::RollUp, credits, false), 0.0));
    assert!(n0 > 50 && y0 < H as f32 / 2.0, "{n0} at {y0}");

    // A ticker: right to left.
    let ticker = project(CrawlKind::CrawlLeft, "BREAKING NEWS — MARKETS UP — WEATHER FINE", true);
    assert_eq!(lit(&at(&ticker, 0.0)).0, 0);
    // Coming in on the right, going out on the left.
    let span = |img: &[u8]| {
        let xs: Vec<u32> = img.chunks_exact(4).enumerate().filter(|(_, p)| p[0] > 128).map(|(i, _)| i as u32 % W).collect();
        (xs.iter().copied().min().unwrap_or(W), xs.iter().copied().max().unwrap_or(0))
    };
    let (first, _) = span(&at(&ticker, 0.5));
    let (_, last) = span(&at(&ticker, 3.5));
    assert!(first > W / 3 && last < W * 2 / 3, "in from the right ({first}), out on the left ({last})");

    // The sci-fi crawl: rising and shrinking into the distance, faded out at the top.
    let scifi = project(CrawlKind::SciFi, &["ONWARD AND OUTWARD"; 6].join("\n"), true);
    let early = at(&scifi, 1.0);
    let later = at(&scifi, 2.5);
    let (ne, _, ye) = lit(&early);
    let (nl, _, yl) = lit(&later);
    assert!(ne > 50 && nl > 20, "{ne} {nl}");
    assert!(ye > yl, "moving up the frame: {ye} → {yl}");
    // Tipped back: lines higher up (farther away) are narrower.
    let full = at(&scifi, 2.0);
    let widths: Vec<u32> = (0..H)
        .filter_map(|y| {
            let xs: Vec<u32> = (0..W).filter(|&x| full[((y * W + x) * 4) as usize] > 128).collect();
            Some(xs.last()? - xs.first()?)
        })
        .collect();
    assert!(widths.len() > 20, "{} rows", widths.len());
    let (top, bottom) = (widths[..5].iter().max().copied().unwrap_or(0), widths[widths.len() - 5..].iter().max().copied().unwrap_or(0));
    assert!(bottom as f32 > top as f32 * 1.15, "narrower farther away: top {top}, bottom {bottom}");
    let top_rows = (0..H / 8).flat_map(|y| (0..W).map(move |x| (x, y))).filter(|&(x, y)| later[((y * W + x) * 4) as usize] > 128).count();
    assert_eq!(top_rows, 0, "faded out before the top of the frame");
}
