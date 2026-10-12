//! Push a snapshot of files to a Git remote without a Git client.
//!
//! Builds loose objects (blobs, trees, one commit), packs them, and speaks the
//! v1 smart-HTTP `git-receive-pack` protocol, the only push protocol Cloudflare
//! Artifacts accepts. Transport is the caller's: a Worker uses `fetch`, tests
//! pipe the same bytes through `git receive-pack --stateless-rpc`.
//!
//! The pack holds every object of the snapshot, without deltas; Git accepts
//! objects the remote already has. The ref update names the remote's current
//! commit as its old value, so a concurrent push makes this one fail instead
//! of overwriting it.

use sha1::{Digest, Sha1};
use std::collections::BTreeMap;

/// A 20-byte SHA-1 object id.
pub type Oid = [u8; 20];

pub const ZERO: Oid = [0; 20];

pub fn hex(oid: &Oid) -> String {
    oid.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn parse_hex(s: &str) -> Option<Oid> {
    if s.len() != 40 {
        return None;
    }
    let mut oid = [0u8; 20];
    for (i, byte) in oid.iter_mut().enumerate() {
        *byte = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(oid)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Commit = 1,
    Tree = 2,
    Blob = 3,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Commit => "commit",
            Kind::Tree => "tree",
            Kind::Blob => "blob",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Object {
    pub kind: Kind,
    pub data: Vec<u8>,
}

impl Object {
    pub fn id(&self) -> Oid {
        let mut h = Sha1::new();
        h.update(format!("{} {}\0", self.kind.name(), self.data.len()).as_bytes());
        h.update(&self.data);
        h.finalize().into()
    }
}

/// A tree entry's mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// 100644
    Regular,
    /// 100755
    Executable,
    /// 120000: the bytes are the link target.
    Symlink,
    /// 160000: the bytes are the 20-byte commit id; no object is written.
    Gitlink,
}

impl Mode {
    fn git(self) -> &'static str {
        match self {
            Mode::Regular => "100644",
            Mode::Executable => "100755",
            Mode::Symlink => "120000",
            Mode::Gitlink => "160000",
        }
    }
}

/// A tree entry: its bytes and mode.
pub type File = (Vec<u8>, Mode);

/// The objects of a tree of `files` (paths separated by `/`), and its id.
/// Objects are deduplicated by id.
pub fn tree(files: &BTreeMap<String, File>) -> Result<(Oid, Vec<Object>), String> {
    #[derive(Default)]
    struct Dir {
        files: BTreeMap<String, (Oid, Mode)>,
        dirs: BTreeMap<String, Dir>,
    }
    fn write(dir: &Dir, out: &mut BTreeMap<Oid, Object>) -> Oid {
        // Git orders entries by name, comparing a directory as `name/`.
        let mut entries: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        for (name, (oid, mode)) in &dir.files {
            entries.push((name.as_bytes().to_vec(), entry(mode.git(), name, oid)));
        }
        for (name, sub) in &dir.dirs {
            let oid = write(sub, out);
            let mut key = name.as_bytes().to_vec();
            key.push(b'/');
            entries.push((key, entry("40000", name, &oid)));
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        let object = Object {
            kind: Kind::Tree,
            data: entries.into_iter().flat_map(|e| e.1).collect(),
        };
        let oid = object.id();
        out.insert(oid, object);
        oid
    }
    fn entry(mode: &str, name: &str, oid: &Oid) -> Vec<u8> {
        let mut e = format!("{mode} {name}\0").into_bytes();
        e.extend_from_slice(oid);
        e
    }
    let mut root = Dir::default();
    let mut objects = BTreeMap::new();
    for (path, (bytes, mode)) in files {
        let parts: Vec<&str> = path.split('/').collect();
        if parts
            .iter()
            .any(|p| p.is_empty() || *p == "." || *p == ".." || p.contains('\0'))
        {
            return Err(format!("invalid path {path:?}"));
        }
        let oid = if *mode == Mode::Gitlink {
            bytes
                .as_slice()
                .try_into()
                .map_err(|_| format!("{path:?}: a submodule needs a 20-byte commit id"))?
        } else {
            let blob = Object {
                kind: Kind::Blob,
                data: bytes.clone(),
            };
            let oid = blob.id();
            objects.insert(oid, blob);
            oid
        };
        let mut dir = &mut root;
        for part in &parts[..parts.len() - 1] {
            if dir.files.contains_key(*part) {
                return Err(format!("{path:?} is below a file"));
            }
            dir = dir.dirs.entry(part.to_string()).or_default();
        }
        let name = parts[parts.len() - 1];
        if dir.dirs.contains_key(name) {
            return Err(format!("{path:?} is also a directory"));
        }
        dir.files.insert(name.to_string(), (oid, *mode));
    }
    let oid = write(&root, &mut objects);
    Ok((oid, objects.into_values().collect()))
}

/// A commit of `tree` with an optional parent, by `who` (`Name <email>`) at
/// `seconds` since the epoch, UTC.
pub fn commit(tree: &Oid, parent: Option<&Oid>, who: &str, seconds: u64, message: &str) -> Object {
    let mut text = format!("tree {}\n", hex(tree));
    if let Some(p) = parent {
        text.push_str(&format!("parent {}\n", hex(p)));
    }
    text.push_str(&format!(
        "author {who} {seconds} +0000\ncommitter {who} {seconds} +0000\n\n{message}"
    ));
    if !message.ends_with('\n') {
        text.push('\n');
    }
    Object {
        kind: Kind::Commit,
        data: text.into_bytes(),
    }
}

/// A version-2 packfile holding `objects`, undeltified.
pub fn pack(objects: &[Object]) -> Vec<u8> {
    let mut out = b"PACK".to_vec();
    out.extend_from_slice(&2u32.to_be_bytes());
    out.extend_from_slice(&(objects.len() as u32).to_be_bytes());
    for o in objects {
        // Type and size: 3 type bits and 4 size bits, then 7 bits per byte.
        let mut size = o.data.len();
        let mut byte = ((o.kind as u8) << 4) | (size & 0x0f) as u8;
        size >>= 4;
        while size != 0 {
            out.push(byte | 0x80);
            byte = (size & 0x7f) as u8;
            size >>= 7;
        }
        out.push(byte);
        out.extend_from_slice(&miniz_oxide::deflate::compress_to_vec_zlib(&o.data, 6));
    }
    let digest: [u8; 20] = Sha1::digest(&out).into();
    out.extend_from_slice(&digest);
    out
}

fn pkt(line: &[u8]) -> Vec<u8> {
    let mut out = format!("{:04x}", line.len() + 4).into_bytes();
    out.extend_from_slice(line);
    out
}

/// Split pkt-lines; a flush is an empty line.
fn pkt_lines(mut bytes: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    let mut out = Vec::new();
    while !bytes.is_empty() {
        let len = std::str::from_utf8(bytes.get(..4).ok_or("truncated pkt-line")?)
            .ok()
            .and_then(|s| usize::from_str_radix(s, 16).ok())
            .ok_or("malformed pkt-line length")?;
        if len == 0 {
            out.push(Vec::new());
            bytes = &bytes[4..];
            continue;
        }
        if len < 4 || len > bytes.len() {
            return Err("pkt-line length out of range".into());
        }
        let mut line = bytes[4..len].to_vec();
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        out.push(line);
        bytes = &bytes[len..];
    }
    Ok(out)
}

/// The refs a receive-pack advertisement lists (`GET info/refs?service=git-receive-pack`).
pub fn advertised_refs(body: &[u8]) -> Result<BTreeMap<String, Oid>, String> {
    let mut refs = BTreeMap::new();
    for line in pkt_lines(body)? {
        if line.is_empty() || line.starts_with(b"#") {
            continue;
        }
        let line = line.split(|b| *b == 0).next().unwrap_or(&[]);
        let text = std::str::from_utf8(line).map_err(|_| "non-UTF-8 ref line")?;
        let (oid, name) = text.split_once(' ').ok_or("malformed ref line")?;
        if name == "capabilities^{}" {
            continue;
        }
        refs.insert(
            name.to_string(),
            parse_hex(oid).ok_or("malformed object id")?,
        );
    }
    Ok(refs)
}

/// The body of `POST git-receive-pack` that moves `refname` from `old` (or
/// creates it, with `ZERO`) to `new`, carrying `pack`.
pub fn push_request(refname: &str, old: &Oid, new: &Oid, pack: &[u8]) -> Vec<u8> {
    let mut out = pkt(format!("{} {} {refname}\0report-status\n", hex(old), hex(new)).as_bytes());
    out.extend_from_slice(b"0000");
    out.extend_from_slice(pack);
    out
}

/// Whether the server unpacked and accepted the update of `refname`.
pub fn push_result(body: &[u8], refname: &str) -> Result<(), String> {
    let lines = pkt_lines(body)?;
    let mut unpacked = false;
    for line in &lines {
        let text = String::from_utf8_lossy(line);
        if let Some(status) = text.strip_prefix("unpack ") {
            if status != "ok" {
                return Err(format!("remote could not unpack: {status}"));
            }
            unpacked = true;
        } else if let Some(r) = text.strip_prefix("ok ") {
            if r == refname && unpacked {
                return Ok(());
            }
        } else if let Some(rest) = text.strip_prefix("ng ") {
            return Err(format!("remote refused {rest}"));
        }
    }
    Err(format!("no status for {refname} in the push report"))
}
