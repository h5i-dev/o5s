//! `o5s` command-line tool.
//!
//! Exit codes: 0 success, 1 error, 2 not found / absent, 3 replace conflict,
//! 4 forbidden by namespace rule, 5 unauthorized.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use clap::{Parser, Subcommand};
use o5s_core::{Error, Hash, Name, Observed, ReplaceOutcome, Repository, Rule};
use o5s_repo::LocalRepo;
use o5s_repo::PackConfigExt as _;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "o5s", version, about = "Content-addressed repository: put/get objects, move names with OCC")]
struct Cli {
    /// Repository directory.
    #[arg(short = 'C', long, env = "O5S_REPO", default_value = ".")]
    repo: PathBuf,
    /// Talk to a o5s server instead of a local repository (e.g. <https://o5s.example.com>).
    #[arg(long, env = "O5S_REMOTE")]
    remote: Option<String>,
    /// Bearer token for --remote.
    #[arg(long, env = "O5S_TOKEN", hide_env_values = true)]
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
        /// zstd level for byte-grouped records (record codec 2, used when a put
        /// carries a grouping width; default 6).
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
    /// Store a file (or stdin with `-`), print its hash. How it is stored comes from
    /// the namespace profile of `--for-name` (chunker, derive from the name's current
    /// object, the profile's front); without it, the store's defaults. Works with
    /// `--remote` too. Doesn't move any name.
    ///
    /// Several FILEs (and `--batch`) are stored by one writer and sealed together:
    /// one hash per line, in input order, printed only once all of them are durable.
    Put {
        /// Files to store, in order; `-` is stdin. Default: stdin.
        files: Vec<PathBuf>,
        /// Also read paths from stdin, one per line, after any FILEs.
        #[arg(long)]
        batch: bool,
        /// With `--batch`: paths are NUL-terminated instead (e.g. `find -print0`).
        #[arg(short = 'z', long, requires = "batch")]
        null: bool,
        /// Apply NAME's namespace profile (see `init --profile/--namespace`).
        #[arg(long, value_name = "NAME", conflicts_with = "encoding")]
        for_name: Option<Name>,
        /// Low-level override of the encoding, as KEY=VALUE (repeatable): `chunker=ID`
        /// (fastcdc|xet), `group-width=N`, `derive-from=HASH`, `patch=xor|xor-anchored`
        /// (with derive-from; default xor-anchored). Storage only, never the hash.
        #[arg(long = "encoding", value_name = "KEY=VALUE")]
        encoding: Vec<String>,
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
    /// Atomically update several names (one OCC transaction): `NAME=HASH` sets,
    /// `NAME=-` deletes. Every target must be in the read set (`--expect`).
    Update {
        #[arg(required = true, value_name = "NAME=HASH|NAME=-")]
        updates: Vec<String>,
        /// Read set: `NAME=absent|VERSION` (repeatable).
        #[arg(long = "expect", value_name = "NAME=STATE")]
        expect: Vec<String>,
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
    /// File paths as names (needs no repository): the canonical, reversible encoding.
    Name {
        #[command(subcommand)]
        cmd: NameCmd,
    },
    /// Seal open packs (make written objects durable and visible).
    Flush,
    /// Repository statistics.
    Stat,
    /// Verify every stored object (CRC + content hash) and every ref. Exit 1 on problems.
    Fsck,
    /// Provenance: record derivations; lineage, impact and why of a HASH or NAME.
    Prov {
        #[command(subcommand)]
        cmd: ProvCmd,
    },
    /// Signed attestations (in-toto Statement v1 in DSSE envelopes) about a HASH or NAME.
    Attest {
        #[command(subcommand)]
        cmd: AttestCmd,
    },
    /// Promote: move DST to the object SRC names, if the policy given by the flags
    /// holds (one OCC transaction, recorded as a `promote` provenance record).
    /// With --remote, the server's `[[promotion]]` policy applies instead (and the
    /// policy flags are refused).
    Promote {
        src: Name,
        dst: Name,
        /// A key that must have signed an attestation about the object: hex Ed25519
        /// public key (repeatable). Local only.
        #[arg(long = "require-key", value_name = "PUBHEX")]
        require_key: Vec<String>,
        /// Required attestations must have this predicate type. Local only.
        #[arg(long)]
        predicate_type: Option<String>,
        /// The object must have a provenance record that produced it. Local only.
        #[arg(long)]
        require_provenance: bool,
        /// Who is promoting (recorded in the audit record).
        #[arg(long, default_value = "cli")]
        actor: String,
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
        /// Repack every pack, even fully live ones (recompresses records written
        /// at a fast ingest level, e.g. `init --zstd-level 1`, into similarity frames).
        #[arg(long)]
        repack_all: bool,
    },
}

#[derive(Subcommand)]
enum NameCmd {
    /// Print the name of each file PATH (one per line, in order). Per `/`-separated
    /// segment, every byte outside `[A-Za-z0-9._-]` becomes `+XX` (uppercase hex),
    /// and a `.` or `..` segment has its dots escaped. Fails on an empty path or
    /// segment, or a name over 1024 bytes.
    Encode {
        #[arg(required = true)]
        paths: Vec<std::ffi::OsString>,
    },
    /// Print the file path each NAME encodes (one per line, in order). Strict: only
    /// the canonical encoding of a path decodes.
    Decode {
        #[arg(required = true)]
        names: Vec<String>,
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
    /// Everything TARGET (a HASH or a NAME) was derived from. Prints `<hash> <names...>`.
    Lineage { target: String },
    /// Everything derived from TARGET (what to re-verify if it's bad).
    Impact { target: String },
    /// How TARGET was made: the direct producing records, then the full lineage.
    Why { target: String },
}

#[derive(Subcommand)]
enum AttestCmd {
    /// Generate an Ed25519 signing key: writes a 32-byte seed (hex) to FILE and
    /// prints `<keyid> <public key hex>`.
    Keygen { file: PathBuf },
    /// Sign and store an attestation about TARGET (a HASH or a NAME) with the key
    /// in KEYFILE. Prints the envelope's hash.
    Create {
        target: String,
        #[arg(long = "key", value_name = "KEYFILE")]
        key: PathBuf,
        /// in-toto predicate type URI.
        #[arg(long, default_value = "https://o5s.dev/attestation/v1")]
        predicate_type: String,
        /// Predicate JSON file (default `{}`).
        #[arg(long)]
        predicate: Option<PathBuf>,
        /// Subject name in the statement (default: TARGET).
        #[arg(long)]
        subject_name: Option<String>,
    },
    /// List the attestations about TARGET: `<predicate type> <key ids...>`.
    List { target: String },
    /// Verify TARGET's attestations against trusted public keys (hex, repeatable).
    /// Prints `<predicate type> <verified key ids...>`; exit 1 if none verifies.
    Verify {
        target: String,
        #[arg(long = "trust", value_name = "PUBHEX", required = true)]
        trust: Vec<String>,
    },
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
        let mut cfg = o5s_repo::PackConfig::default();
        cfg.apply_overrides(*chunk_threshold, *pack_cap, *zstd_level);
        if let Some(l) = grouped_zstd_level {
            cfg.grouped_zstd_level = *l;
        }
        let store = if *refs_on_backend { o5s_repo::RefsStore::Backend } else { o5s_repo::RefsStore::Local };
        let parse = |c: &str| match c {
            "fastcdc" => Ok(o5s_repo::ChunkerChoice::Fastcdc),
            "xet" => Ok(o5s_repo::ChunkerChoice::Xet),
            other => Err(Error::Parse(format!("unknown chunker {other:?} (fastcdc or xet)"))),
        };
        let mut chunking = o5s_repo::ChunkingSection::default();
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
            let mut p = o5s_repo::Profile::default();
            for item in kv.split(',').filter(|i| !i.is_empty()) {
                let (k, v) = item
                    .split_once(':')
                    .ok_or_else(|| Error::Parse(format!("--profile {spec:?}: bad item {item:?}")))?;
                match k {
                    "front" => p.front = Some(v.to_string()),
                    "chunker" => p.chunker = Some(v.to_string()),
                    "derive" => {
                        p.derive = match v {
                            "none" => o5s_repo::DerivePolicy::None,
                            "previous" => o5s_repo::DerivePolicy::Previous,
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
            &o5s_repo::RepoConfig {
                format: o5s_repo::FORMAT_VERSION,
                pack: (&cfg).into(),
                refs: o5s_repo::RefsSection { store },
                chunking,
                profiles,
                namespaces,
                read: o5s_repo::ReadSection {
                    verify: match read_verify.as_deref() {
                        None => o5s_core::ReadVerify::Strict,
                        Some(m) => o5s_core::ReadVerify::parse(m)
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
    if let Cmd::Name { cmd } = &cli.cmd {
        let mut out = std::io::stdout().lock();
        match cmd {
            NameCmd::Encode { paths } => {
                use std::os::unix::ffi::OsStrExt;
                for p in paths {
                    writeln!(out, "{}", Name::from_path_bytes(p.as_bytes())?)?;
                }
            }
            NameCmd::Decode { names } => {
                for n in names {
                    let n = Name::new(n.as_str())?;
                    out.write_all(&n.to_path_bytes()?)?;
                    out.write_all(b"\n")?;
                }
            }
        }
        return Ok(Outcome::Ok);
    }
    let remote = match &cli.remote {
        Some(url) => Some(o5s_client::RemoteRepo::connect(url, cli.token.clone())?),
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
        Cmd::Init { .. } | Cmd::Name { .. } => unreachable!("handled above"),
        Cmd::Put { files, batch, null, for_name, encoding } => {
            let mut paths = files;
            if batch {
                if paths.iter().any(|p| p.as_os_str() == "-") {
                    return Err(Error::Parse("put --batch reads paths from stdin, so no FILE can be `-`".into()));
                }
                paths.extend(read_path_list(&mut std::io::stdin().lock(), if null { b'\0' } else { b'\n' })?);
            } else if paths.is_empty() {
                paths.push(PathBuf::from("-"));
            }
            if paths.iter().filter(|p| p.as_os_str() == "-").count() > 1 {
                return Err(Error::Parse("put: stdin (`-`) can be given only once".into()));
            }
            let enc = if encoding.is_empty() { None } else { Some(parse_encoding(&encoding)?) };
            let mut hashes = Vec::with_capacity(paths.len());
            for path in &paths {
                let mut src: Box<dyn Read> = if path.as_os_str() == "-" {
                    Box::new(std::io::stdin().lock())
                } else {
                    let f = std::fs::File::open(path).map_err(|e| in_file(path, e.into()))?;
                    Box::new(std::io::BufReader::new(f))
                };
                let h = match (&for_name, &local, &remote, &enc) {
                    (Some(n), Some(l), _, _) => l.put_named(n, &mut src),
                    (Some(n), None, Some(rr), _) => rr.put_named(n, &mut src),
                    (Some(_), None, None, _) => return Err(Error::Unsupported("no repository")),
                    (None, _, _, Some(e)) => r.put_encoded(&mut src, e),
                    (None, _, _, None) => r.put_stream(&mut src),
                };
                hashes.push(h.map_err(|e| in_file(path, e))?);
            }
            // One seal for the whole batch; nothing is printed before it, so every
            // printed hash is durable.
            r.flush()?;
            for h in hashes {
                writeln!(out, "{h}")?;
            }
        }
        Cmd::Get { hash, range, output, read_verify } => {
            let range = range.as_deref().map(parse_range).transpose()?;
            let mut src = match read_verify.as_deref() {
                None => r.open(&hash, range)?,
                Some(m) => {
                    let mode = o5s_core::ReadVerify::parse(m)
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
                    let tmp = p.with_extension("o5s-partial");
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
        Cmd::Search { prefix, words } if remote.is_some() => {
            let Some(rr) = &remote else { unreachable!("guarded") };
            let q = match prefix {
                Some(p) => o5s_client::Search::Prefix(p),
                None => o5s_client::Search::Text(words.join(" ")),
            };
            for hit in rr.search(q)? {
                writeln!(out, "{} {}", hit.object, hit.names.join(" "))?;
            }
        }
        Cmd::Search { prefix, words } => {
            let log = Name::new(o5s_prov::DEFAULT_LOG_REF)?;
            let idx = o5s_prov::search::SearchIndex::build(r, &log)?;
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
        Cmd::Retention { dry_run } => {
            let Some(l) = &local else {
                return Err(Error::Unsupported("retention is only available for local repositories"));
            };
            let log = Name::new(o5s_prov::DEFAULT_LOG_REF)?;
            let rep = o5s_prov::retention::apply(l, &log, dry_run)?;
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
        Cmd::Update { updates, expect } => {
            let mut ups = Vec::new();
            for u in &updates {
                let (n, v) = u
                    .split_once('=')
                    .ok_or_else(|| Error::Parse(format!("expected NAME=HASH or NAME=-, got {u:?}")))?;
                ups.push((Name::new(n)?, if v == "-" { None } else { Some(v.parse::<Hash>()?) }));
            }
            let mut rs = Vec::new();
            for e in &expect {
                let (n, o) =
                    e.split_once('=').ok_or_else(|| Error::Parse(format!("--expect wants NAME=STATE, got {e:?}")))?;
                rs.push((Name::new(n)?, parse_observed(o)?));
            }
            match r.update_many(&ups, &rs)? {
                o5s_core::ReplaceManyOutcome::Ok { versions } => {
                    writeln!(out, "ok {}", versions.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(","))?
                }
                o5s_core::ReplaceManyOutcome::Conflict { stale } => {
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
            let log = Name::new(o5s_prov::DEFAULT_LOG_REF)?;
            let cmd_kind = match &cmd {
                ProvCmd::Lineage { .. } => "lineage",
                _ => "other",
            };
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
                    let rec = o5s_prov::Record { op, params, inputs: roles(inputs)?, outputs: roles(outputs)?, meta };
                    for h in o5s_prov::record(r, &log, &[rec])? {
                        writeln!(out, "{h}")?;
                    }
                }
                ProvCmd::Lineage { target } | ProvCmd::Impact { target } => {
                    let lineage = matches!(cmd_kind, "lineage");
                    let set: std::collections::BTreeSet<Hash> = match &remote {
                        // The server answers from its own index (no full log download).
                        Some(rr) => {
                            let id = ref_id(&target);
                            (if lineage { rr.lineage(&id)? } else { rr.impact(&id)? }).into_iter().collect()
                        }
                        None => {
                            let h = resolve_target(r, &target)?;
                            let p = o5s_prov::Provenance::load(r, &log)?;
                            if lineage { p.lineage(&h)? } else { p.impact(&h)? }
                        }
                    };
                    let names = names_by_hash(r)?;
                    for x in set {
                        let ns = names.get(&x).map(|v| v.join(" ")).unwrap_or_default();
                        writeln!(out, "{x} {ns}")?;
                    }
                }
                ProvCmd::Why { target } => {
                    let hash = resolve_target(r, &target)?;
                    let p = o5s_prov::Provenance::load(r, &log)?;
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
            }
        }
        Cmd::Attest { cmd } => {
            let log = Name::new(o5s_prov::DEFAULT_LOG_REF)?;
            match cmd {
                AttestCmd::Keygen { file } => {
                    let mut seed = [0u8; 32];
                    std::fs::File::open("/dev/urandom")?.read_exact(&mut seed)?;
                    write_secret(&file, &hex(&seed))?;
                    let signer = o5s_attest::Ed25519Signer::from_seed(&seed);
                    use o5s_attest::Signer as _;
                    writeln!(out, "{} {}", signer.keyid(), hex(&signer.verifier().public()))?;
                }
                AttestCmd::Create { target, key, predicate_type, predicate, subject_name } => {
                    let h = resolve_target(r, &target)?;
                    let seed = read_seed(&key)?;
                    let predicate = match predicate {
                        Some(p) => serde_json::from_slice(&std::fs::read(p)?)
                            .map_err(|e| Error::Parse(format!("--predicate: {e}")))?,
                        None => serde_json::json!({}),
                    };
                    let st = o5s_attest::Statement {
                        subjects: vec![o5s_attest::Subject { name: subject_name.unwrap_or(target), hash: h }],
                        predicate_type,
                        predicate,
                    };
                    let signer = o5s_attest::Ed25519Signer::from_seed(&seed);
                    let env = o5s_attest::Envelope::sign(&st, &[&signer]);
                    match &remote {
                        Some(rr) => writeln!(out, "{}", rr.attest(&env)?.0)?,
                        None => writeln!(out, "{}", o5s_attest::attest(r, &log, &env)?)?,
                    }
                }
                AttestCmd::List { target } => {
                    for env in envelopes(r, &remote, &log, &target)? {
                        let pt = env.statement().map(|s| s.predicate_type).unwrap_or_else(|_| "?".into());
                        let ids: Vec<&str> = env.signatures.iter().map(|(k, _)| k.as_str()).collect();
                        writeln!(out, "{pt} {}", ids.join(" "))?;
                    }
                }
                AttestCmd::Verify { target, trust } => {
                    let verifiers = trust.iter().map(|t| parse_pubkey(t)).collect::<Result<Vec<_>, _>>()?;
                    let vs: Vec<&dyn o5s_attest::Verifier> =
                        verifiers.iter().map(|v| v as &dyn o5s_attest::Verifier).collect();
                    let mut any = false;
                    for env in envelopes(r, &remote, &log, &target)? {
                        let ok = env.verified_by(&vs);
                        if !ok.is_empty() {
                            any = true;
                            let pt = env.statement().map(|s| s.predicate_type).unwrap_or_else(|_| "?".into());
                            writeln!(out, "{pt} {}", ok.join(" "))?;
                        }
                    }
                    if !any {
                        out.flush()?;
                        return Err(Error::Corrupt("no attestation verifies with the trusted keys".into()));
                    }
                }
            }
        }
        Cmd::Promote { src, dst, require_key, predicate_type, require_provenance, actor } => {
            let log = Name::new(o5s_prov::DEFAULT_LOG_REF)?;
            let outcome = if let Some(rr) = &remote {
                // The server applies its own [[promotion]] policy; local policy flags would
                // suggest otherwise, so they are refused here rather than ignored.
                if !require_key.is_empty() || predicate_type.is_some() || require_provenance {
                    return Err(Error::Parse(
                        "with --remote the server's [[promotion]] policy applies; drop --require-key/--predicate-type/--require-provenance".into(),
                    ));
                }
                rr.promote(&src, &dst)?
            } else {
                let verifiers = require_key.iter().map(|t| parse_pubkey(t)).collect::<Result<Vec<_>, _>>()?;
                use o5s_attest::Verifier as _;
                let policy = o5s_attest::Policy {
                    required_keys: verifiers.iter().map(|v| v.keyid()).collect(),
                    predicate_type,
                    require_provenance,
                };
                let vs: Vec<&dyn o5s_attest::Verifier> =
                    verifiers.iter().map(|v| v as &dyn o5s_attest::Verifier).collect();
                o5s_attest::promote(r, &log, &src, &dst, &policy, &vs, &actor)?
            };
            match outcome {
                o5s_attest::PromoteOutcome::Promoted { object, record } => {
                    writeln!(out, "promoted {object} record {record}")?
                }
                o5s_attest::PromoteOutcome::Refused(why) => {
                    for w in &why {
                        writeln!(out, "refused {w}")?;
                    }
                    out.flush()?;
                    return Err(Error::Forbidden {
                        name: dst.clone(),
                        rule: Rule::Mutable,
                        why: "promotion policy not satisfied",
                    });
                }
                o5s_attest::PromoteOutcome::Conflict => {
                    writeln!(out, "conflict")?;
                    return Ok(Outcome::Conflict);
                }
            }
        }
        Cmd::Gc { grace, unsafe_short_grace, dry_run, no_similarity_repack, repack_all } => {
            let repo = local_only("gc is only available for local repositories (run it on the server host)")?;
            let base = repo.gc_options();
            let opts = o5s_repo::GcOptions {
                grace: parse_duration(&grace)?,
                allow_short_grace: unsafe_short_grace,
                dry_run,
                similarity_repack: base.similarity_repack && !no_similarity_repack,
                repack_all,
                ..base
            };
            // Provenance records keep their inputs alive (but not their outputs).
            let tracer = o5s_prov::ProvTracer { inner: o5s_repo::OpaqueObjects };
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
            let g = o5s_repo::gc::gc_state(repo)?;
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
            eprintln!("o5s: {e}");
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
fn builtin_fronts() -> o5s_core::Result<o5s_repo::FrontRegistry> {
    o5s_repo::FrontRegistry::new().with(std::sync::Arc::new(o5s_weights::SafetensorsFront))
}

/// A HASH (64 hex digits) or a NAME (resolved to the object it points at).
fn resolve_target(r: &dyn Repository, t: &str) -> Result<Hash, Error> {
    if let Ok(h) = t.parse::<Hash>() {
        return Ok(h);
    }
    let n = Name::new(t)?;
    r.resolve(&n)?.map(|v| v.hash).ok_or(Error::NotFound(Hash([0; 32])))
}

/// Names currently pointing at each object.
fn names_by_hash(r: &dyn Repository) -> Result<std::collections::HashMap<Hash, Vec<String>>, Error> {
    let mut m: std::collections::HashMap<Hash, Vec<String>> = std::collections::HashMap::new();
    for (n, v) in r.list_refs("")? {
        m.entry(v.hash).or_default().push(n.to_string());
    }
    Ok(m)
}

/// `--encoding KEY=VALUE...` → the Front API's [`o5s_core::Encoding`].
fn parse_encoding(items: &[String]) -> Result<o5s_core::Encoding, Error> {
    let mut enc = o5s_core::Encoding::default();
    let mut derive_from: Option<Hash> = None;
    let mut patch = o5s_core::PatchFormat::XorAnchored;
    for it in items {
        let (k, v) =
            it.split_once('=').ok_or_else(|| Error::Parse(format!("--encoding wants KEY=VALUE, got {it:?}")))?;
        match k {
            "chunker" => enc.chunker = Some(v.to_string()),
            "group-width" => {
                enc.group_width = Some(v.parse().map_err(|_| Error::Parse(format!("group-width: {v:?} is not 1..16")))?)
            }
            "derive-from" => derive_from = Some(v.parse()?),
            "patch" => {
                patch = match v {
                    "xor" => o5s_core::PatchFormat::Xor,
                    "xor-anchored" => o5s_core::PatchFormat::XorAnchored,
                    _ => return Err(Error::Parse(format!("patch: {v:?} (xor or xor-anchored)"))),
                }
            }
            _ => {
                return Err(Error::Parse(format!(
                    "--encoding: unknown key {k:?} (chunker, group-width, derive-from, patch)"
                )));
            }
        }
    }
    enc.derive = derive_from.map(|base| o5s_core::Derive { base, patch, segments: Vec::new() });
    Ok(enc)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex32(s: &str) -> Option<[u8; 32]> {
    let s = s.trim();
    // Exactly 64 hex digits (`from_str_radix` alone would accept a '+' sign).
    if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(out)
}

fn read_seed(p: &std::path::Path) -> Result<[u8; 32], Error> {
    unhex32(&std::fs::read_to_string(p)?)
        .ok_or_else(|| Error::Parse(format!("{}: expected 64 hex digits", p.display())))
}

fn parse_pubkey(s: &str) -> Result<o5s_attest::Ed25519Verifier, Error> {
    let b = unhex32(s).ok_or_else(|| Error::Parse(format!("public key {s:?}: expected 64 hex digits")))?;
    o5s_attest::Ed25519Verifier::from_public(&b)
}

/// Writes a secret file readable only by its owner (created new; never overwritten).
fn write_secret(p: &std::path::Path, contents: &str) -> Result<(), Error> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(p)?;
    f.write_all(contents.as_bytes())?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(())
}

/// A server reference id for TARGET: `blake3:<hex>` for a hash, `name:<name>` otherwise.
fn ref_id(t: &str) -> String {
    if t.parse::<Hash>().is_ok() { format!("blake3:{t}") } else { format!("name:{t}") }
}

/// TARGET's attestation envelopes, locally or from the server.
fn envelopes(
    r: &dyn Repository,
    remote: &Option<o5s_client::RemoteRepo>,
    log: &Name,
    target: &str,
) -> Result<Vec<o5s_attest::Envelope>, Error> {
    match remote {
        Some(rr) => Ok(rr.attestations(&ref_id(target))?.into_iter().map(|(_, e, _)| e).collect()),
        None => o5s_attest::attestations(r, log, &resolve_target(r, target)?),
    }
}

/// Splits a path list (from `put --batch`) on `sep`. Empty entries are skipped;
/// on Unix, paths are taken as raw bytes, so any file name works.
fn read_path_list(src: &mut dyn Read, sep: u8) -> Result<Vec<PathBuf>, Error> {
    let mut buf = Vec::new();
    src.read_to_end(&mut buf)?;
    let mut out = Vec::new();
    for item in buf.split(|b| *b == sep).filter(|s| !s.is_empty()) {
        #[cfg(unix)]
        out.push(PathBuf::from(<std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(item)));
        #[cfg(not(unix))]
        out.push(PathBuf::from(
            std::str::from_utf8(item).map_err(|_| Error::Parse("put --batch: path is not UTF-8".into()))?,
        ));
    }
    Ok(out)
}

/// Names the file an I/O error came from (for multi-file `put`). Other errors
/// keep their kind, so the exit code still says what went wrong.
fn in_file(path: &std::path::Path, e: Error) -> Error {
    match e {
        Error::Io(io) => Error::Io(std::io::Error::new(io.kind(), format!("{}: {io}", path.display()))),
        e => e,
    }
}
