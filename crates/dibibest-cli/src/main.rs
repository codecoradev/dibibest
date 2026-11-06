//! dibibest — convert database schemas bolak-balik.
//!
//! `dibibest convert` reads DBML/SQL/Prisma/ERD-JSON, emits any of them (ERD-JSON for the
//! canvas), and always prints a fidelity report: what survived, what dropped.
//! `dibibest render` draws the schema's ERD as an SVG image.

use std::io::Read;
use std::process::ExitCode;

use dibibest_core::{convert_explicit, FormatId};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!();
            eprintln!("{}", usage());
            ExitCode::from(2)
        }
    }
}

fn usage() -> String {
    let ids = dibibest_core::default_registry().ids().join(", ");
    format!(
        "usage: dibibest convert [--from <format>] --to <format> [file | -]\n       \
         dibibest render [--from <format>] [--theme dark|light] [file | -]   (SVG to stdout)\n       \
         dibibest formats\nformats: {ids}"
    )
}

fn run(args: &[String]) -> Result<ExitCode, String> {
    match args.first().map(String::as_str) {
        None => Err("no command given".into()),
        Some("-h" | "--help" | "help") => {
            println!("{}", usage());
            Ok(ExitCode::SUCCESS)
        }
        Some("formats") => {
            println!("{}", dibibest_core::default_registry().ids().join("\n"));
            Ok(ExitCode::SUCCESS)
        }
        Some("convert") => cmd_convert(&args[1..]),
        Some("render") => cmd_render(&args[1..]),
        Some(other) => Err(format!("unknown command: {other}")),
    }
}

fn cmd_convert(args: &[String]) -> Result<ExitCode, String> {
    let mut from: Option<String> = None;
    let mut to: Option<String> = None;
    let mut file: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--from" => {
                i += 1;
                from = Some(args.get(i).ok_or("--from needs a value")?.clone());
            }
            "--to" => {
                i += 1;
                to = Some(args.get(i).ok_or("--to needs a value")?.clone());
            }
            other if file.is_none() => file = Some(other.to_string()),
            other => return Err(format!("unexpected argument: {other}")),
        }
        i += 1;
    }
    let to = to.ok_or("--to is required (e.g. --to sql)")?;

    let src = read_source(file.as_deref())?;

    let result = match from {
        Some(f) => convert_explicit(&src, &FormatId::new(f), &FormatId::new(&to)),
        None => dibibest_core::registry_convert(&src, &FormatId::new(&to)),
    }
    .map_err(|e| e.to_string())?;

    print!("{}", result.output);
    eprint!("{}", render_fidelity(&result));
    Ok(ExitCode::SUCCESS)
}

/// `file` or stdin (`None` / `-`).
fn read_source(file: Option<&str>) -> Result<String, String> {
    match file {
        None | Some("-") => {
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .map_err(|e| format!("reading stdin: {e}"))?;
            Ok(buf)
        }
        Some(path) => std::fs::read_to_string(path).map_err(|e| format!("reading {path}: {e}")),
    }
}

fn cmd_render(args: &[String]) -> Result<ExitCode, String> {
    use dibibest_core::diagram::{render_source, RenderOptions, Theme};
    let mut from: Option<String> = None;
    let mut theme = Theme::Dark;
    let mut file: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--from" => {
                i += 1;
                from = Some(args.get(i).ok_or("--from needs a value")?.clone());
            }
            "--theme" => {
                i += 1;
                let t = args.get(i).ok_or("--theme needs a value")?;
                theme =
                    Theme::parse(t).ok_or_else(|| format!("unknown theme: {t} (dark | light)"))?;
            }
            other if file.is_none() => file = Some(other.to_string()),
            other => return Err(format!("unexpected argument: {other}")),
        }
        i += 1;
    }
    let src = read_source(file.as_deref())?;
    let opts = RenderOptions {
        theme,
        ..Default::default()
    };
    let svg = render_source(&src, from.as_deref(), &opts).map_err(|e| e.to_string())?;
    println!("{}", svg.svg);
    Ok(ExitCode::SUCCESS)
}

fn render_fidelity(c: &dibibest_core::Conversion) -> String {
    let f = &c.fidelity;
    let row = |label: &str, o: dibibest_core::Outcome| {
        if o.lossless() {
            format!("  {label:<8} {}/{} ✓\n", o.kept, o.total)
        } else {
            format!("  {label:<8} {}/{} ⚠ DROPPED\n", o.kept, o.total)
        }
    };
    let mut s = String::from("\n-- fidelity --\n");
    s.push_str(&row("tables", f.tables));
    s.push_str(&row("columns", f.columns));
    s.push_str(&row("refs", f.refs));
    s.push_str(&row("indexes", f.indexes));
    if f.idempotent {
        s.push_str("  round-trip in source format: lossless ✓\n");
    } else {
        s.push_str("  round-trip in source format: NOT idempotent ⚠\n");
    }
    for d in &f.dropped {
        s.push_str(&format!("  note: {d}\n"));
    }
    s
}
