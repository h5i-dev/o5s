//! `pico2peta` command-line tool.
//!
//! Exit codes: 0 success, 1 error, 2 not found / absent, 3 replace conflict,
//! 4 forbidden by namespace rule.

use clap::{Parser, Subcommand};
use pico2peta_core::{Error, Hash, Name, Observed, ReplaceOutcome, Repository, Rule};
use pico2peta_repo::LocalRepo;
use pico2peta_repo::PackConfigExt as _;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "pico2peta", version, about = "Content-addressed repository: put/get objects, move names with OCC")]
struct Cli {
    /// Repository directory.
    #[arg(short = 'C', long, env = "PICO2PETA_REPO", default_value = ".")]
    repo: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a repository.
    Init {
        /// Objects at least this large are chunked (bytes).
        #[arg(long)]
        chunk_threshold: Option<usize>,
        /// Seal open packs at this size (bytes).
        #[arg(long)]
        pack_cap: Option<u64>,
        /// zstd level for pack records (0 = off).
        #[arg(long)]
        zstd_level: Option<i32>,
    },
    /// Store a file (or stdin with `-`), print its hash.
    Put {
        #[arg(default_value = "-")]
        file: PathBuf,
    },
    /// Write an object (or a byte range of it) to stdout or a file.
    Get {
        hash: Hash,
        /// Byte range `START..END` (end exclusive).
        #[arg(long)]
        range: Option<String>,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Print an object's size in bytes.
    Size { hash: Hash },
    /// Print `<hash> <version>` for a name.
    Resolve { name: Name },
    /// Compare-and-swap a name.
    Replace {
        name: Name,
        hash: Hash,
        /// Observed state of NAME: `absent` or a version number.
        #[arg(long)]
        expect: String,
        /// Further read-set entries `OTHER=absent|VERSION` (repeatable).
        #[arg(long = "read")]
        reads: Vec<String>,
    },
    /// List refs under a prefix.
    Refs {
        #[arg(default_value = "")]
        prefix: String,
    },
    /// Namespace rules.
    Ns {
        #[command(subcommand)]
        cmd: NsCmd,
    },
    /// Seal open packs (make written objects durable and visible).
    Flush,
    /// Repository statistics.
    Stat,
}

#[derive(Subcommand)]
enum NsCmd {
    /// Set the rule for a prefix: mutable | immutable | head-release.
    Set { prefix: String, rule: Rule },
    /// List rules.
    List,
}

fn parse_observed(s: &str) -> Result<Observed, Error> {
    if s == "absent" {
        return Ok(Observed::Absent);
    }
    s.parse::<u64>()
        .ok()
        .filter(|v| *v > 0)
        .map(Observed::Version)
        .ok_or_else(|| Error::Parse(format!("expected `absent` or a version >= 1, got {s:?}")))
}

fn parse_range(s: &str) -> Result<std::ops::Range<u64>, Error> {
    let bad = || Error::Parse(format!("range must be START..END, got {s:?}"));
    let (a, b) = s.split_once("..").ok_or_else(bad)?;
    Ok(a.parse().map_err(|_| bad())?..b.parse().map_err(|_| bad())?)
}

enum Outcome {
    Ok,
    Absent,
    Conflict,
}

fn run(cli: Cli) -> Result<Outcome, Error> {
    if let Cmd::Init { chunk_threshold, pack_cap, zstd_level } = &cli.cmd {
        let mut cfg = pico2peta_repo::PackConfig::default();
        cfg.apply_overrides(*chunk_threshold, *pack_cap, *zstd_level);
        LocalRepo::init(&cli.repo, &cfg)?;
        println!("initialized {}", cli.repo.display());
        return Ok(Outcome::Ok);
    }
    let repo = LocalRepo::open(&cli.repo)?;
    let r: &dyn Repository = &repo;
    let mut out = std::io::stdout().lock();
    match cli.cmd {
        Cmd::Init { .. } => unreachable!("handled above"),
        Cmd::Put { file } => {
            let h = if file.as_os_str() == "-" {
                r.put_stream(&mut std::io::stdin().lock())?
            } else {
                r.put_stream(&mut std::io::BufReader::new(std::fs::File::open(&file)?))?
            };
            r.flush()?;
            writeln!(out, "{h}")?;
        }
        Cmd::Get { hash, range, output } => {
            let range = range.as_deref().map(parse_range).transpose()?;
            let mut src = r.open(&hash, range)?;
            let mut copy = |w: &mut dyn Write| -> Result<(), Error> {
                let mut buf = vec![0u8; 1 << 20];
                loop {
                    let n = src.read(&mut buf).map_err(io_to_core)?;
                    if n == 0 {
                        return Ok(());
                    }
                    w.write_all(&buf[..n])?;
                }
            };
            match output {
                Some(p) => {
                    // Write to a temp file and rename, so a failed or corrupt read leaves no partial file.
                    let tmp = p.with_extension("p2p-partial");
                    let mut f = std::fs::File::create(&tmp)?;
                    if let Err(e) = copy(&mut f) {
                        let _ = std::fs::remove_file(&tmp);
                        return Err(e);
                    }
                    f.sync_all()?;
                    std::fs::rename(&tmp, &p)?;
                }
                None => copy(&mut out)?,
            }
        }
        Cmd::Size { hash } => writeln!(out, "{}", r.size(&hash)?)?,
        Cmd::Resolve { name } => match r.resolve(&name)? {
            Some(v) => writeln!(out, "{} {}", v.hash, v.version)?,
            None => return Ok(Outcome::Absent),
        },
        Cmd::Replace { name, hash, expect, reads } => {
            let mut rs = vec![(name.clone(), parse_observed(&expect)?)];
            for e in reads {
                let (n, o) =
                    e.split_once('=').ok_or_else(|| Error::Parse(format!("--read wants NAME=STATE, got {e:?}")))?;
                rs.push((Name::new(n)?, parse_observed(o)?));
            }
            match r.replace(&name, &rs, hash)? {
                ReplaceOutcome::Ok { version } => writeln!(out, "ok {version}")?,
                ReplaceOutcome::Conflict { stale } => {
                    for (n, o) in stale {
                        let cur = match o {
                            Observed::Absent => "absent".to_string(),
                            Observed::Version(v) => v.to_string(),
                        };
                        writeln!(out, "conflict {n} {cur}")?;
                    }
                    return Ok(Outcome::Conflict);
                }
            }
        }
        Cmd::Refs { prefix } => {
            for (n, v) in r.list_refs(&prefix)? {
                writeln!(out, "{} {} {}", v.hash, v.version, n)?;
            }
        }
        Cmd::Ns { cmd: NsCmd::Set { prefix, rule } } => {
            r.set_rule(&prefix, rule)?;
        }
        Cmd::Ns { cmd: NsCmd::List } => {
            for (p, rule) in r.rules()? {
                writeln!(out, "{p} {}", rule.as_str())?;
            }
        }
        Cmd::Flush => r.flush()?,
        Cmd::Stat => {
            let s = repo.pack_stats()?;
            writeln!(out, "backend        {}", repo.backend().describe())?;
            writeln!(out, "sealed_packs   {}", s.sealed_packs)?;
            writeln!(out, "objects        {} ({} blobs, {} manifests)", s.objects, s.blobs, s.manifests)?;
            writeln!(out, "logical_bytes  {}", s.logical_blob_bytes)?;
            writeln!(out, "refs           {}", r.list_refs("")?.len())?;
        }
    }
    out.flush()?;
    Ok(Outcome::Ok)
}

fn io_to_core(e: std::io::Error) -> Error {
    match e.downcast::<Error>() {
        Ok(inner) => inner,
        Err(e) => Error::Io(e),
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(Outcome::Ok) => ExitCode::SUCCESS,
        Ok(Outcome::Absent) => ExitCode::from(2),
        Ok(Outcome::Conflict) => ExitCode::from(3),
        Err(e) => {
            // A closed stdout (e.g. `| head`) is not an error worth reporting.
            if let Error::Io(io) = &e
                && io.kind() == std::io::ErrorKind::BrokenPipe
            {
                return ExitCode::SUCCESS;
            }
            eprintln!("pico2peta: {e}");
            match e {
                Error::NotFound(_) | Error::KeyNotFound(_) => ExitCode::from(2),
                Error::Forbidden { .. } => ExitCode::from(4),
                _ => ExitCode::from(1),
            }
        }
    }
}
