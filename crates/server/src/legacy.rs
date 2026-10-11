//! Import a pre-kernel `state.json` snapshot into the registry log.
//!
//! Each legacy patch body is re-encoded canonically and must reproduce its
//! stored SHA-256 identity, so imported patches keep their identities and
//! handles. Channel membership keeps its order. The legacy format recorded
//! only whether checks last passed, not who reported them; a passing report is
//! imported with reporter `IMPORTED`. The imported database is projected back
//! and compared with the legacy repositories before anything is written.
use o5s_platform::{Database, Effect, Repository, body, repository_key};
use o5s_registry as kernel;
use std::{collections::BTreeMap, path::Path};

/// The reporter recorded for imported passing check reports.
pub const IMPORTED: u64 = u64::MAX;

#[derive(serde::Deserialize)]
struct Legacy {
    tenants: BTreeMap<u64, BTreeMap<String, Repository>>,
}

fn action(name: &str) -> Result<u64, String> {
    Ok(match name {
        "create" => kernel::CREATE,
        "fork" => kernel::FORK,
        "propose" => kernel::PROPOSE,
        "approve" => kernel::APPROVE,
        "check" => kernel::CHECK,
        "merge" => kernel::MERGE,
        other => return Err(format!("unknown legacy event action {other:?}")),
    })
}

/// The registry effects that reproduce one legacy repository.
fn repository(tenant: u64, name: &str, repo: &Repository) -> Result<Vec<kernel::Write>, String> {
    use kernel::Write as W;
    let mut writes = Vec::new();
    let mut channel_ids = BTreeMap::new();
    let mut order = vec!["main".to_string()];
    order.extend(
        repo.channels
            .keys()
            .filter(|c| c.as_str() != "main")
            .cloned(),
    );
    for (i, channel) in order.iter().enumerate() {
        let id = i as u64 + 1;
        channel_ids.insert(channel.clone(), id);
        let members = repo.channels.get(channel).cloned().unwrap_or_default();
        writes.push(W::PutChannel(kernel::Channel {
            id,
            label: channel.as_bytes().to_vec(),
            size: members.len() as u64,
        }));
    }
    let mut paths: BTreeMap<String, bool> = BTreeMap::new();
    let mut path_rows = Vec::new();
    let mut last = 0;
    for (id, patch) in &repo.patches {
        if *id != last + 1 {
            return Err(format!("{name}: patch handles are not contiguous at {id}"));
        }
        last = *id;
        let encoded = body::encode(
            patch.author,
            &patch.intent,
            &patch.dependencies,
            &patch.edits,
            &patch.line_edits,
            &patch.graph_edits,
            repo,
        )
        .map_err(|e| format!("{name}: patch {id}: {e}"))?;
        if body::hash(&encoded) != patch.hash {
            return Err(format!(
                "{name}: patch {id} does not reproduce its identity"
            ));
        }
        writes.push(W::PutPatch(kernel::PatchRow {
            id: *id,
            hash: patch.hash.as_bytes().to_vec(),
            author: patch.author,
        }));
        writes.push(W::PutBody(kernel::Body {
            patch: *id,
            bytes: encoded,
        }));
        for d in &patch.dependencies {
            writes.push(W::PutDependency(kernel::Dependency {
                patch: *id,
                dependency: *d,
            }));
        }
        for reviewer in &patch.approvals {
            writes.push(W::PutApproval(kernel::Approval {
                patch: *id,
                reviewer: *reviewer,
            }));
        }
        if patch.checks_passed {
            writes.push(W::PutCheck(kernel::CheckReport {
                patch: *id,
                passed: true,
                reporter: IMPORTED,
            }));
        }
        let uses = patch
            .edits
            .iter()
            .map(|e| (&e.path, false))
            .chain(patch.graph_edits.iter().map(|e| (&e.path, true)));
        for (path, graph) in uses {
            match paths.get(path) {
                Some(kind) if *kind != graph => {
                    return Err(format!("{name}: {path} mixes graph and legacy edits"));
                }
                Some(_) => {}
                None => {
                    paths.insert(path.clone(), graph);
                    path_rows.push(W::PutPath(kernel::PathKind {
                        id: path_rows.len() as u64 + 1,
                        path: path.as_bytes().to_vec(),
                        graph,
                    }));
                }
            }
        }
    }
    let path_count = path_rows.len() as u64;
    writes.extend(path_rows);
    for channel in &order {
        let id = channel_ids[channel];
        for (position, patch) in repo.channels.get(channel).into_iter().flatten().enumerate() {
            writes.push(W::PutMember(kernel::Member {
                channel: id,
                position: position as u64,
                patch: *patch,
            }));
        }
    }
    for (i, event) in repo.events.iter().enumerate() {
        let channel = match &event.channel {
            Some(c) => Some(
                *channel_ids
                    .get(c)
                    .ok_or_else(|| format!("{name}: event names unknown channel {c}"))?,
            ),
            None => None,
        };
        writes.push(W::PutEvent(kernel::Event {
            seq: i as u64 + 1,
            actor: event.actor,
            action: action(&event.action)?,
            patch: event.patch,
            channel,
        }));
    }
    writes.push(W::SetRepository(kernel::Repository {
        org: tenant,
        label: name.as_bytes().to_vec(),
        created: true,
        patches: last,
        channels: order.len() as u64,
        paths: path_count,
        events: repo.events.len() as u64,
    }));
    Ok(writes)
}

/// The effects of a legacy database, checked by projecting them back.
pub fn convert(json: &[u8]) -> Result<Vec<Effect>, String> {
    let legacy: Legacy = serde_json::from_slice(json).map_err(|e| format!("legacy state: {e}"))?;
    let mut effects = Vec::new();
    for (tenant, repos) in &legacy.tenants {
        if *tenant >= kernel::REPOSITORY_SCOPE {
            return Err(format!("tenant {tenant} is outside the organization range"));
        }
        let mut listings = Vec::new();
        for (name, repo) in repos {
            if !o5s_platform::valid_name(name) {
                return Err(format!("invalid repository name {name:?}"));
            }
            let scope = repository_key(*tenant, name);
            listings.push(kernel::Write::PutListing(kernel::Listing {
                repo: scope,
                label: name.as_bytes().to_vec(),
            }));
            effects.push(Effect {
                scope,
                writes: repository(*tenant, name, repo)?,
            });
        }
        effects.push(Effect {
            scope: *tenant,
            writes: listings,
        });
    }
    let mut db = Database::default();
    db.commit(&effects).map_err(|e| e.0)?;
    for (tenant, repos) in &legacy.tenants {
        for (name, repo) in repos {
            let imported = &db.views[&repository_key(*tenant, name)];
            if imported.patches != repo.patches
                || imported.channels != repo.channels
                || imported.events != repo.events
            {
                return Err(format!(
                    "{name}: imported view differs from the legacy repository"
                ));
            }
        }
    }
    Ok(effects)
}

/// Write `root/registry.log` from `root/state.json`, then keep the legacy file
/// as `state.json.imported`. Refuses if a log already exists.
pub fn import(root: &Path) -> Result<usize, String> {
    let log = root.join(crate::local::LOG);
    if log.exists() {
        return Err(format!("{} already exists", log.display()));
    }
    let legacy = root.join("state.json");
    let json = std::fs::read(&legacy).map_err(|e| format!("{}: {e}", legacy.display()))?;
    let effects = convert(&json)?;
    let count = effects.len();
    let staged = root.join("registry.log.import");
    crate::local::write_log(&staged, effects).map_err(|e| e.to_string())?;
    std::fs::rename(&staged, &log).map_err(|e| e.to_string())?;
    std::fs::rename(&legacy, root.join("state.json.imported")).map_err(|e| e.to_string())?;
    std::fs::File::open(root)
        .and_then(|d| d.sync_all())
        .map_err(|e| e.to_string())?;
    Ok(count)
}
