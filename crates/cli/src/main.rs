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
        /// zstd level for byte-grouped model-weight records (default 6; 15 gets
        /// about the full per-tensor gain on bf16 at ~12 MB/s per core).
        #[arg(long)]
        grouped_zstd_level: Option<i32>,
        /// Store refs as a create-only log on the object backend instead of a
        /// local journal (lets several hosts write one repository on shared storage).
        #[arg(long)]
        refs_on_backend: bool,
        /// Chunker for a namespace, as PREFIX=CHUNKER (fastcdc or xet), e.g.
        /// `--chunk models/=xet`. Repeatable; the longest matching prefix wins.
        /// Used by `put --for-name`.
        #[arg(long = "chunk", value_name = "PREFIX=CHUNKER")]
        chunk: Vec<String>,
        /// Chunker for everything else (default fastcdc).
        #[arg(long, value_name = "CHUNKER")]
        chunk_default: Option<String>,
        /// A domain profile, as NAME=key:value,... with keys front, chunker,
        /// derive (none|previous), keep_versions (N) and only_if_regenerable. Example:
        /// `--profile weights=front:safetensors,derive:previous`. Repeatable.
        #[arg(long = "profile", value_name = "NAME=SPEC")]
        profile: Vec<String>,
        /// Assign a profile to a name prefix, as PREFIX=PROFILE (e.g.
        /// `--namespace models/=weights`). Repeatable; longest prefix wins.
        #[arg(long = "namespace", value_name = "PREFIX=PROFILE")]
        namespace: Vec<String>,
        /// Default read verification: "strict" (default) or "eof" (see `get
        /// --read-verify`).
        #[arg(long, value_name = "strict|eof")]
        read_verify: Option<String>,
        /// Let individual reads ask for `--read-verify eof`.
        #[arg(long)]
        allow_eof_reads: bool,
    },
    /// Store a file (or stdin with `-`), print its hash.
    Put {
        #[arg(default_value = "-")]
        file: PathBuf,
        /// Store as an XOR delta against this object (new versions of model
        /// weights). safetensors files are aligned per tensor.
        #[arg(long, conflicts_with = "delta_from_ref")]
        delta_base: Option<Hash>,
        /// Store as an XOR delta against the object NAME currently points at
        /// (e.g. the previous checkpoint of `models/heads/main`). Falls back to a
        /// plain put if NAME doesn't exist.
        #[arg(long)]
        delta_from_ref: Option<Name>,
        /// Chunk with the chunker configured for NAME's namespace (`init
        /// --chunk`). Doesn't create NAME. Local repositories only.
        #[arg(long, value_name = "NAME", conflicts_with_all = ["delta_base", "delta_from_ref"])]
        for_name: Option<Name>,
        /// Don't byte-group safetensors weights before compressing (by default
        /// bf16/fp16/fp32 checkpoints are stored with the grouped codec; this
        /// changes only the stored size, never the hash).
        #[arg(long)]
        no_byte_group: bool,
    },
    /// Write an object (or a byte range of it) to stdout or a file.
    Get {
        hash: Hash,
        /// Byte range `START..END` (end exclusive).
        #[arg(long)]
        range: Option<String>,
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Verification for this read: "strict" (every chunk's hash before its
        /// bytes are returned) or "eof" (record CRCs, plus the whole-object hash
        /// at EOF, so CRC-valid crafted bytes can be written before the error).
        /// "eof" needs `[read] allow_eof`. Default: the repository's setting.
        #[arg(long, value_name = "strict|eof")]
        read_verify: Option<String>,
    },
    /// Search names and metadata: full text (all words must match), or
    /// `--prefix P` for names under a prefix. Prints `<hash> <names...>`.
    Search {
        #[arg(long, conflicts_with = "words")]
        prefix: Option<String>,
        words: Vec<String>,
    },
    /// Everything NAME's object was derived from (provenance, across fronts).
    Lineage { name: Name },
    /// Everything derived from NAME's object (what to re-verify if it's bad).
    Impact { name: Name },
    /// Apply the profiles' retention (keep_versions, only_if_regenerable):
    /// delete old versions' names so GC can collect them. Local only.
    Retention {
        #[arg(long)]
        dry_run: bool,
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
    /// Verify every stored object (CRC + content hash) and every ref. Exit 1 on problems.
    Fsck,
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
        /// Copy live records verbatim instead of laying them out by content
        /// similarity (the default, also settable with `[gc] similarity_repack`).
        #[arg(long)]
        no_similarity_repack: bool,
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
    if let Cmd::Init {
        chunk_threshold,
        pack_cap,
        zstd_level,
        grouped_zstd_level,
        refs_on_backend,
        chunk,
        chunk_default,
        profile,
        namespace,
        read_verify,
        allow_eof_reads,
    } = &cli.cmd
    {
        if cli.remote.is_some() {
            return Err(Error::Unsupported("init is only available for local repositories"));
        }
        let mut cfg = pico2peta_repo::PackConfig::default();
        cfg.apply_overrides(*chunk_threshold, *pack_cap, *zstd_level);
        if let Some(l) = grouped_zstd_level {
            cfg.grouped_zstd_level = *l;
        }
        let store =
            if *refs_on_backend { pico2peta_repo::RefsStore::Backend } else { pico2peta_repo::RefsStore::Local };
        let parse = |c: &str| match c {
            "fastcdc" => Ok(pico2peta_repo::ChunkerChoice::Fastcdc),
            "xet" => Ok(pico2peta_repo::ChunkerChoice::Xet),
            other => Err(Error::Parse(format!("unknown chunker {other:?} (fastcdc or xet)"))),
        };
        let mut chunking = pico2peta_repo::ChunkingSection::default();
        if let Some(c) = chunk_default {
            chunking.default = parse(c)?;
        }
        for spec in chunk {
            let (prefix, c) = spec
                .split_once('=')
                .ok_or_else(|| Error::Parse(format!("--chunk {spec:?}: expected PREFIX=CHUNKER")))?;
            chunking.prefixes.insert(prefix.to_string(), parse(c)?);
        }
        let mut profiles = std::collections::BTreeMap::new();
        for spec in profile {
            let (name, kv) = spec
                .split_once('=')
                .ok_or_else(|| Error::Parse(format!("--profile {spec:?}: expected NAME=key:value,...")))?;
            let mut p = pico2peta_repo::Profile::default();
            for item in kv.split(',').filter(|i| !i.is_empty()) {
                let (k, v) = item
                    .split_once(':')
                    .ok_or_else(|| Error::Parse(format!("--profile {spec:?}: bad item {item:?}")))?;
                match k {
                    "front" => p.front = Some(v.to_string()),
                    "chunker" => p.chunker = Some(v.to_string()),
                    "derive" => {
                        p.derive = match v {
                            "none" => pico2peta_repo::DerivePolicy::None,
                            "previous" => pico2peta_repo::DerivePolicy::Previous,
                            _ => return Err(Error::Parse(format!("--profile {spec:?}: derive is none or previous"))),
                        }
                    }
                    "keep_versions" => {
                        p.keep_versions = Some(
                            v.parse()
                                .map_err(|_| Error::Parse(format!("--profile {spec:?}: keep_versions is a number")))?,
                        )
                    }
                    "only_if_regenerable" => {
                        p.only_if_regenerable = v.parse().map_err(|_| {
                            Error::Parse(format!("--profile {spec:?}: only_if_regenerable is true or false"))
                        })?
                    }
                    _ => return Err(Error::Parse(format!("--profile {spec:?}: unknown key {k:?}"))),
                }
            }
            profiles.insert(name.to_string(), p);
        }
        let mut namespaces = std::collections::BTreeMap::new();
        for spec in namespace {
            let (prefix, p) = spec
                .split_once('=')
                .ok_or_else(|| Error::Parse(format!("--namespace {spec:?}: expected PREFIX=PROFILE")))?;
            namespaces.insert(prefix.to_string(), p.to_string());
        }
        for (n, p) in &profiles {
            if let Some(f) = &p.front
                && builtin_fronts()?.get(f).is_none()
            {
                return Err(Error::Parse(format!("--profile {n}: unknown front {f:?}")));
            }
        }
        LocalRepo::init_config(
            &cli.repo,
            &pico2peta_repo::RepoConfig {
                format: pico2peta_repo::FORMAT_VERSION,
                pack: (&cfg).into(),
                refs: pico2peta_repo::RefsSection { store },
                chunking,
                profiles,
                namespaces,
                read: pico2peta_repo::ReadSection {
                    verify: match read_verify.as_deref() {
                        None => pico2peta_core::ReadVerify::Strict,
                        Some(m) => pico2peta_core::ReadVerify::parse(m)
                            .ok_or_else(|| Error::Parse(format!("--read-verify {m:?}: expected strict or eof")))?,
                    },
                    allow_eof: *allow_eof_reads,
                },
                gc: Default::default(),
            },
        )?;
        println!("initialized {}", cli.repo.display());
        return Ok(Outcome::Ok);
    }
    let remote = match &cli.remote {
        Some(url) => Some(pico2peta_client::RemoteRepo::connect(url, cli.token.clone())?),
        None => None,
    };
    let local = if remote.is_none() { Some(LocalRepo::open(&cli.repo)?.with_fronts(builtin_fronts()?)?) } else { None };
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
        Cmd::Put { file, delta_base, delta_from_ref, for_name, no_byte_group } => {
            let delta_base = match (delta_base, delta_from_ref) {
                (Some(b), _) => Some(b),
                (None, Some(n)) => r.resolve(&n)?.map(|v| v.hash),
                (None, None) => None,
            };
            let mut src: Box<dyn Read> = if file.as_os_str() == "-" {
                Box::new(std::io::stdin().lock())
            } else {
                Box::new(std::io::BufReader::new(std::fs::File::open(&file)?))
            };
            let h = match delta_base {
                // Tensor-aligned for safetensors checkpoints; offset-aligned otherwise.
                Some(b) => pico2peta_weights::put_checkpoint_opts(r, &b, &mut src, !no_byte_group)?,
                None => {
                    let (head, width) = pico2peta_weights::sniff(&mut src)?;
                    let width = if no_byte_group { None } else { width };
                    let mut src = std::io::Cursor::new(head).chain(src);
                    match (&for_name, &local, width) {
                        (Some(n), Some(l), w) => l.put_named_hinted(n, &mut src, w)?,
                        (Some(_), None, _) => {
                            return Err(Error::Unsupported("put --for-name is only available for local repositories"));
                        }
                        (None, _, Some(w)) => r.put_stream_grouped(&mut src, w)?,
                        (None, _, None) => r.put_stream(&mut src)?,
                    }
                }
            };
            r.flush()?;
            writeln!(out, "{h}")?;
        }
        Cmd::Get { hash, range, output, read_verify } => {
            let range = range.as_deref().map(parse_range).transpose()?;
            let mut src = match read_verify.as_deref() {
                None => r.open(&hash, range)?,
                Some(m) => {
                    let mode = pico2peta_core::ReadVerify::parse(m)
                        .ok_or_else(|| Error::Parse(format!("--read-verify {m:?}: expected strict or eof")))?;
                    r.open_verified(&hash, range, mode)?
                }
            };
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
        Cmd::Search { prefix, words } => {
            let log = Name::new(pico2peta_prov::DEFAULT_LOG_REF)?;
            let idx = pico2peta_prov::search::SearchIndex::build(r, &log)?;
            match prefix {
                Some(p) => {
                    for (n, h) in idx.prefix(&p) {
                        writeln!(out, "{h} {n}")?;
                    }
                }
                None => {
                    for hit in idx.text(&words.join(" ")) {
                        writeln!(out, "{} {}", hit.hash, hit.names.join(" "))?;
                    }
                }
            }
        }
        Cmd::Lineage { name } => {
            let log = Name::new(pico2peta_prov::DEFAULT_LOG_REF)?;
            for hit in pico2peta_prov::search::lineage_named(r, &log, &name)? {
                writeln!(out, "{} {}", hit.hash, hit.names.join(" "))?;
            }
        }
        Cmd::Impact { name } => {
            let log = Name::new(pico2peta_prov::DEFAULT_LOG_REF)?;
            for hit in pico2peta_prov::search::impact_named(r, &log, &name)? {
                writeln!(out, "{} {}", hit.hash, hit.names.join(" "))?;
            }
        }
        Cmd::Retention { dry_run } => {
            let Some(l) = &local else {
                return Err(Error::Unsupported("retention is only available for local repositories"));
            };
            let log = Name::new(pico2peta_prov::DEFAULT_LOG_REF)?;
            let rep = pico2peta_prov::retention::apply(l, &log, dry_run)?;
            for n in &rep.deleted {
                writeln!(out, "{} {n}", if dry_run { "would-delete" } else { "deleted" })?;
            }
            for n in &rep.kept_by_rule {
                writeln!(out, "kept-by-rule {n}")?;
            }
            for n in &rep.kept_not_regenerable {
                writeln!(out, "kept-not-regenerable {n}")?;
            }
            for n in &rep.conflicts {
                writeln!(out, "conflict {n}")?;
            }
        }
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
                    let producers = p.producers(&hash)?;
                    for (rh, rec) in &producers {
                        writeln!(out, "record {rh} op={}", rec.op)?;
                        for (role, h) in &rec.inputs {
                            writeln!(out, "  in  {role:<12} {h}")?;
                        }
                    }
                    for h in p.lineage(&hash)? {
                        writeln!(out, "lineage {h}")?;
                    }
                    if producers.is_empty() {
                        return Ok(Outcome::Absent);
                    }
                }
                ProvCmd::Impact { hash } => {
                    let p = pico2peta_prov::Provenance::load(r, &log)?;
                    for h in p.impact(&hash)? {
                        writeln!(out, "{h}")?;
                    }
                }
            }
        }
        Cmd::Gc { grace, unsafe_short_grace, dry_run, no_similarity_repack } => {
            let repo = local_only("gc is only available for local repositories (run it on the server host)")?;
            let base = repo.gc_options();
            let opts = pico2peta_repo::GcOptions {
                grace: parse_duration(&grace)?,
                allow_short_grace: unsafe_short_grace,
                dry_run,
                similarity_repack: base.similarity_repack && !no_similarity_repack,
                ..base
            };
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
        Cmd::Fsck => {
            let repo = local_only("fsck is only available for local repositories (run it on the server host)")?;
            let rep = repo.fsck()?;
            writeln!(out, "objects   {} ({} bytes verified)", rep.objects_checked, rep.bytes_checked)?;
            writeln!(out, "refs      {}", rep.refs_checked)?;
            for e in &rep.errors {
                writeln!(out, "ERROR     {e}")?;
            }
            if !rep.ok() {
                out.flush()?;
                return Err(Error::Corrupt(format!("fsck found {} problem(s)", rep.errors.len())));
            }
            writeln!(out, "ok")?;
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

/// The fronts the CLI opens repositories with.
fn builtin_fronts() -> pico2peta_core::Result<pico2peta_repo::FrontRegistry> {
    pico2peta_repo::FrontRegistry::new().with(std::sync::Arc::new(pico2peta_weights::SafetensorsFront))
}
