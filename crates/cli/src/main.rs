//! `pico2peta` command-line tool.
//!
//! Exit codes: 0 success, 1 error, 2 not found / absent, 3 replace conflict,
//! 4 forbidden by namespace rule, 5 unauthorized.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

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
    /// Talk to a pico2peta server instead of a local repository (e.g. <https://p2p.example.com>).
    #[arg(long, env = "P2P_REMOTE")]
    remote: Option<String>,
    /// Bearer token for --remote.
    #[arg(long, env = "P2P_TOKEN", hide_env_values = true)]
    token: Option<String>,
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
        /// Store as an XOR delta against this object (for new versions of
        /// model weights with the same layout).
        #[arg(long)]
        delta_base: Option<Hash>,
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
    /// Delete a name (OCC). Immutable/released names can't be deleted.
    Delete {
        name: Name,
        /// Observed state of NAME: `absent` or a version number.
        #[arg(long)]
        expect: String,
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
    /// Provenance: record derivations, query lineage and impact.
    Prov {
        #[command(subcommand)]
        cmd: ProvCmd,
    },
    /// Run whichever garbage-collection phases are due (plan → sweep → delete).
    /// Safe with concurrent writers. Run it periodically (e.g. from cron).
    /// Roots are the current refs; objects are treated as opaque.
    Gc {
        /// Minimum time between phases (e.g. 30m, 1h, 2d). Must be at least
        /// 10m unless --unsafe-short-grace is given.
        #[arg(long, default_value = "1h")]
        grace: String,
        /// Allow a grace period under 10 minutes. Only safe with no active writers.
        #[arg(long)]
        unsafe_short_grace: bool,
        /// Report what would happen without changing anything.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum ProvCmd {
    /// Record that OP turned inputs into outputs. Roles are given as ROLE=HASH.
    Record {
        op: String,
        #[arg(long = "in")]
        inputs: Vec<String>,
        #[arg(long = "out")]
        outputs: Vec<String>,
        /// Hash of a parameters object.
        #[arg(long)]
        params: Option<Hash>,
        /// Metadata KEY=VALUE (repeatable).
        #[arg(long = "meta")]
        meta: Vec<String>,
    },
    /// Show how HASH was made (direct producers), then its full lineage.
    Why { hash: Hash },
    /// List everything derived (transitively) from HASH.
    Impact { hash: Hash },
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

fn parse_duration(s: &str) -> Result<std::time::Duration, Error> {
    let bad = || Error::Parse(format!("duration must look like 90s, 30m, 1h or 2d, got {s:?}"));
    let (num, mult) = match s.char_indices().last() {
        Some((i, 's')) => (&s[..i], 1),
        Some((i, 'm')) => (&s[..i], 60),
        Some((i, 'h')) => (&s[..i], 3600),
        Some((i, 'd')) => (&s[..i], 86400),
        _ if s == "0" => ("0", 1),
        _ => return Err(bad()),
    };
    let n: u64 = num.parse().map_err(|_| bad())?;
    Ok(std::time::Duration::from_secs(n.checked_mul(mult).ok_or_else(bad)?))
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
        if cli.remote.is_some() {
            return Err(Error::Unsupported("init is only available for local repositories"));
        }
        let mut cfg = pico2peta_repo::PackConfig::default();
        cfg.apply_overrides(*chunk_threshold, *pack_cap, *zstd_level);
        LocalRepo::init(&cli.repo, &cfg)?;
        println!("initialized {}", cli.repo.display());
        return Ok(Outcome::Ok);
    }
    let remote = match &cli.remote {
        Some(url) => Some(pico2peta_client::RemoteRepo::connect(url, cli.token.clone())?),
        None => None,
    };
    let local = if remote.is_none() { Some(LocalRepo::open(&cli.repo)?) } else { None };
    let r: &dyn Repository = match (&remote, &local) {
        (Some(rr), _) => rr,
        (None, Some(l)) => l,
        (None, None) => return Err(Error::Unsupported("no repository")),
    };
    let local_only =
        |what: &'static str| -> Result<&LocalRepo, Error> { local.as_ref().ok_or(Error::Unsupported(what)) };
    let mut out = std::io::stdout().lock();
    match cli.cmd {
        Cmd::Init { .. } => unreachable!("handled above"),
        Cmd::Put { file, delta_base } => {
            let mut src: Box<dyn Read> = if file.as_os_str() == "-" {
                Box::new(std::io::stdin().lock())
            } else {
                Box::new(std::io::BufReader::new(std::fs::File::open(&file)?))
            };
            let h = match delta_base {
                Some(b) => r.put_delta(&b, &mut src)?,
                None => r.put_stream(&mut src)?,
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
        Cmd::Delete { name, expect } => match r.delete_ref(&name, &[(name.clone(), parse_observed(&expect)?)])? {
            ReplaceOutcome::Ok { version } => writeln!(out, "deleted {version}")?,
            ReplaceOutcome::Conflict { stale } => {
                for (n, o) in stale {
                    writeln!(out, "conflict {n} {o:?}")?;
                }
                return Ok(Outcome::Conflict);
            }
        },
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
        Cmd::Prov { cmd } => {
            let log = Name::new(pico2peta_prov::DEFAULT_LOG_REF)?;
            match cmd {
                ProvCmd::Record { op, inputs, outputs, params, meta } => {
                    let roles = |v: Vec<String>| -> Result<std::collections::BTreeMap<String, Hash>, Error> {
                        v.into_iter()
                            .map(|e| {
                                let (k, h) = e
                                    .split_once('=')
                                    .ok_or_else(|| Error::Parse(format!("expected ROLE=HASH, got {e:?}")))?;
                                Ok((k.to_string(), h.parse()?))
                            })
                            .collect()
                    };
                    let meta = meta
                        .into_iter()
                        .map(|e| {
                            e.split_once('=')
                                .map(|(k, v)| (k.to_string(), v.to_string()))
                                .ok_or_else(|| Error::Parse(format!("expected KEY=VALUE, got {e:?}")))
                        })
                        .collect::<Result<_, _>>()?;
                    let rec =
                        pico2peta_prov::Record { op, params, inputs: roles(inputs)?, outputs: roles(outputs)?, meta };
                    for h in pico2peta_prov::record(r, &log, &[rec])? {
                        writeln!(out, "{h}")?;
                    }
                }
                ProvCmd::Why { hash } => {
                    let p = pico2peta_prov::Provenance::load(r, &log)?;
                    for (rh, rec) in p.producers(&hash) {
                        writeln!(out, "record {rh} op={}", rec.op)?;
                        for (role, h) in &rec.inputs {
                            writeln!(out, "  in  {role:<12} {h}")?;
                        }
                    }
                    for h in p.lineage(&hash) {
                        writeln!(out, "lineage {h}")?;
                    }
                    if p.producers(&hash).is_empty() {
                        return Ok(Outcome::Absent);
                    }
                }
                ProvCmd::Impact { hash } => {
                    let p = pico2peta_prov::Provenance::load(r, &log)?;
                    for h in p.impact(&hash) {
                        writeln!(out, "{h}")?;
                    }
                }
            }
        }
        Cmd::Gc { grace, unsafe_short_grace, dry_run } => {
            let opts = pico2peta_repo::GcOptions {
                grace: parse_duration(&grace)?,
                allow_short_grace: unsafe_short_grace,
                dry_run,
                ..Default::default()
            };
            let repo = local_only("gc is only available for local repositories (run it on the server host)")?;
            // Provenance records keep their inputs alive (but not their outputs).
            let tracer = pico2peta_prov::ProvTracer { inner: pico2peta_repo::OpaqueObjects };
            let rep = repo.gc(&opts, &tracer)?;
            if let Some(p) = &rep.planned {
                writeln!(out, "planned        {p}")?;
            }
            writeln!(out, "swept_plans    {}", rep.swept.len())?;
            writeln!(out, "condemned      {} packs", rep.condemned_packs)?;
            writeln!(out, "copied         {} records ({} rescued)", rep.copied_records, rep.rescued_records)?;
            writeln!(out, "deleted        {} packs, {} bytes", rep.deleted_packs, rep.deleted_bytes)?;
            writeln!(out, "live           {} hashes", rep.live_hashes)?;
            for d in &rep.dangling_refs {
                writeln!(out, "DANGLING REF   {d}")?;
            }
        }
        Cmd::Stat => {
            let repo = local_only("stat is only available for local repositories (run it on the server host)")?;
            let s = repo.pack_stats()?;
            writeln!(out, "backend        {}", repo.backend().describe())?;
            writeln!(out, "sealed_packs   {}", s.sealed_packs)?;
            writeln!(out, "objects        {} ({} blobs, {} manifests)", s.objects, s.blobs, s.manifests)?;
            writeln!(out, "logical_bytes  {}", s.logical_blob_bytes)?;
            writeln!(out, "refs           {}", r.list_refs("")?.len())?;
            let g = pico2peta_repo::gc::gc_state(repo)?;
            writeln!(out, "gc_plans       {}", g.get("plans").copied().unwrap_or(0))?;
            writeln!(out, "gc_condemned   {}", g.get("condemned").copied().unwrap_or(0))?;
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
                Error::Unauthorized(_) => ExitCode::from(5),
                _ => ExitCode::from(1),
            }
        }
    }
}
