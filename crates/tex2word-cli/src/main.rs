//! `tex2word` command line (port of `cli.py`):
//!
//! ```text
//! tex2word convert in.tex [-o out.docx] [options]
//! tex2word coverage DIR [-o coverage.html]
//! tex2word benchmark DIR [-o metrics.json] [--fail-on-regression]
//! tex2word to-latex in.docx [-o out.tex] [--no-reconcile]
//! ```
//!
//! Arguments, help and error messages follow Python's `argparse`.

mod argparse;

use std::io::Write;
use std::process::ExitCode;

use argparse::{Exit, Kind, Opt, Parsed, Parser};
use tex2word::benchmark::{benchmark_dir, format_report, glob_tex, relpath, DEVNULL};
use tex2word::ir::json::{self, DumpOptions};
use tex2word::ir::report::aggregate_html;
use tex2word::{convert_file, ConvertOptions, Error};

const COMMANDS: [&str; 4] = ["convert", "coverage", "benchmark", "to-latex"];

const fn opt(dest: &'static str, flags: &'static [&'static str], kind: Kind) -> Opt {
    Opt {
        dest,
        flags,
        kind,
        choices: &[],
    }
}

const HELP: Opt = opt("help", &["-h", "--help"], Kind::Help);

fn main_parser() -> Parser {
    Parser {
        prog: "tex2word",
        help: include_str!("help/help_main.txt"),
        opts: vec![HELP],
        positionals: &["command"],
    }
}

fn sub_parser(command: &str) -> Parser {
    let output = opt("output", &["-o", "--output"], Kind::Value);
    match command {
        "convert" => Parser {
            prog: "tex2word convert",
            help: include_str!("help/help_convert.txt"),
            opts: vec![
                HELP,
                output,
                opt("report", &["--report"], Kind::Value),
                opt("no_manifest", &["--no-manifest"], Kind::Flag),
                opt("number_by_section", &["--number-by-section"], Kind::Flag),
                Opt {
                    choices: &["static", "zotero"],
                    ..opt("citations", &["--citations"], Kind::Value)
                },
                opt("columns", &["--columns"], Kind::Int),
                Opt {
                    choices: &["pure", "latexml"],
                    ..opt("frontend", &["--frontend"], Kind::Value)
                },
                opt(
                    "math_image_fallback",
                    &["--math-image-fallback"],
                    Kind::Flag,
                ),
                opt("csl", &["--csl"], Kind::Value),
                opt("reference_doc", &["--reference-doc"], Kind::Value),
                opt("lang", &["--lang"], Kind::Value),
                opt("quiet", &["-q", "--quiet"], Kind::Flag),
            ],
            positionals: &["input"],
        },
        "coverage" => Parser {
            prog: "tex2word coverage",
            help: include_str!("help/help_coverage.txt"),
            opts: vec![HELP, output],
            positionals: &["directory"],
        },
        "benchmark" => Parser {
            prog: "tex2word benchmark",
            help: include_str!("help/help_benchmark.txt"),
            opts: vec![
                HELP,
                output,
                opt("fail_on_regression", &["--fail-on-regression"], Kind::Flag),
            ],
            positionals: &["directory"],
        },
        _ => Parser {
            prog: "tex2word to-latex",
            help: include_str!("help/help_to-latex.txt"),
            opts: vec![
                HELP,
                output,
                opt("no_reconcile", &["--no-reconcile"], Kind::Flag),
            ],
            positionals: &["input"],
        },
    }
}

/// Parse the command line like `parser.parse_args(argv)`.
fn parse_args(argv: &[String]) -> Result<(String, Parsed), Exit> {
    let main = main_parser();
    // the main parser only knows -h; the first positional is the command and
    // everything after it belongs to the subparser
    let mut extras = Vec::new();
    let mut k = 0;
    while k < argv.len() && argparse::is_option_like(&argv[k]) {
        let a = &argv[k];
        if a == "--" {
            k += 1;
            break;
        }
        if a == "-h" || (a.len() > 2 && "--help".starts_with(a.as_str())) {
            return Err(Exit {
                code: 0,
                stdout: main.help.to_string(),
                stderr: String::new(),
            });
        }
        extras.push(a.clone());
        k += 1;
    }
    let Some(command) = argv.get(k) else {
        return Err(Exit::error(
            &main,
            "the following arguments are required: command",
        ));
    };
    if !COMMANDS.contains(&command.as_str()) {
        let choices: Vec<String> = COMMANDS.iter().map(|c| argparse::repr(c)).collect();
        return Err(Exit::error(
            &main,
            &format!(
                "argument command: invalid choice: {} (choose from {})",
                argparse::repr(command),
                choices.join(", ")
            ),
        ));
    }
    let parsed = sub_parser(command).parse(&argv[k + 1..])?;
    extras.extend(parsed.extras.iter().cloned());
    if !extras.is_empty() {
        return Err(Exit::error(
            &main,
            &format!("unrecognized arguments: {}", extras.join(" ")),
        ));
    }
    Ok((command.clone(), parsed))
}

/// `main(argv)`: the process exit code.
pub fn run(argv: &[String]) -> i32 {
    let (command, args) = match parse_args(argv) {
        Ok(p) => p,
        Err(exit) => {
            print!("{}", exit.stdout);
            eprint!("{}", exit.stderr);
            return exit.code;
        }
    };
    match command.as_str() {
        "convert" => cmd_convert(&args),
        "coverage" => cmd_coverage(&args),
        "benchmark" => cmd_benchmark(&args),
        _ => cmd_to_latex(&args),
    }
}

fn is_not_found(e: &Error) -> bool {
    matches!(e, Error::Io(io) if io.kind() == std::io::ErrorKind::NotFound)
}

fn cmd_convert(args: &Parsed) -> i32 {
    let input = args.get("input").unwrap();
    let opts = ConvertOptions {
        embed_manifest: !args.flag("no_manifest"),
        number_by_section: args.flag("number_by_section"),
        citation_mode: args.get("citations").unwrap_or("static").into(),
        columns: args
            .get("columns")
            .and_then(tex2word::ir::pystr::parse_int)
            .unwrap_or(1),
        frontend: args.get("frontend").unwrap_or("pure").into(),
        math_image_fallback: args.flag("math_image_fallback"),
        csl: args.get("csl").map(str::to_string),
        reference_doc: args.get("reference_doc").map(str::to_string),
        language: args.get("lang").map(str::to_string),
    };
    let (output_path, result) = match convert_file(input, args.get("output"), &opts) {
        Ok(r) => r,
        Err(e) if is_not_found(&e) => {
            eprintln!("error: input file not found: {input}");
            return 2;
        }
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    if let Some(report) = args.get("report") {
        let content = if report.ends_with(".html") {
            result.report.to_html()
        } else {
            result.report.to_json()
        };
        if let Err(e) = std::fs::write(report, content) {
            eprintln!("error: {report}: {e}");
            return 1;
        }
    }
    if !args.flag("quiet") {
        eprintln!("wrote {output_path}");
        eprintln!("{}", result.report.summary());
        for entry in result.report.errors() {
            eprintln!("  error [{}]: {}", entry.construct, entry.message);
        }
    }
    0
}

fn cmd_to_latex(args: &Parsed) -> i32 {
    let input = args.get("input").unwrap();
    let data = match std::fs::read(input) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("error: input file not found: {input}");
            return 2;
        }
        Err(e) => {
            eprintln!("error: {input}: {e}");
            return 1;
        }
    };
    let latex = match tex2word::roundtrip::to_latex(&data, !args.flag("no_reconcile")) {
        Ok(Some(l)) => l,
        Ok(None) => {
            eprintln!("error: no round-trip manifest in this .docx");
            return 1;
        }
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    match args.get("output") {
        Some(out) => {
            if let Err(e) = std::fs::write(out, latex) {
                eprintln!("error: {out}: {e}");
                return 1;
            }
            eprintln!("wrote {out}");
        }
        None => println!("{latex}"),
    }
    0
}

fn cmd_benchmark(args: &Parsed) -> i32 {
    let directory = args.get("directory").unwrap();
    let result = benchmark_dir(directory);
    if result.documents.is_empty() {
        eprintln!("no .tex files under {directory}");
        return 2;
    }
    println!("{}", format_report(&result));
    if let Some(out) = args.get("output") {
        let text = json::dumps(
            &result.to_value(),
            DumpOptions {
                ensure_ascii: true,
                indent: Some(2),
                sort_keys: false,
                compact: false,
            },
        );
        if let Err(e) = std::fs::write(out, text) {
            eprintln!("error: {out}: {e}");
            return 1;
        }
        eprintln!("wrote {out}");
    }
    let agg = &result.aggregate;
    if args.flag("fail_on_regression") && (agg.aborted > 0 || agg.valid < agg.documents) {
        eprintln!("benchmark regression: aborts or invalid output");
        return 1;
    }
    0
}

fn cmd_coverage(args: &Parsed) -> i32 {
    let directory = args.get("directory").unwrap();
    let output = args.get("output").unwrap_or("coverage.html");
    let paths = glob_tex(directory);
    if paths.is_empty() {
        eprintln!("no .tex files under {directory}");
        return 2;
    }
    let opts = ConvertOptions {
        embed_manifest: false,
        ..ConvertOptions::default()
    };
    let mut results = Vec::new();
    for path in &paths {
        // never abort the whole run on one file
        match std::panic::catch_unwind(|| convert_file(path, Some(DEVNULL), &opts)) {
            Ok(Ok((_, result))) => results.push((relpath(path, directory), result.report)),
            Ok(Err(e)) => eprintln!("warning: {path} failed: {e}"),
            Err(_) => eprintln!("warning: {path} failed: internal error"),
        }
    }
    if let Err(e) = std::fs::write(output, aggregate_html(&results)) {
        eprintln!("error: {output}: {e}");
        return 1;
    }
    eprintln!("wrote {output} ({} document(s))", results.len());
    0
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let code = run(&argv);
    let _ = std::io::stdout().flush();
    ExitCode::from(code as u8)
}
