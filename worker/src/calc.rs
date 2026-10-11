//! The calculator's mapping: kernel types to tables, JSON and stored replies.
//! It decides nothing.

use crate::engine::{Api, ReplyCodec, Store};
use axum::http::StatusCode;
use h5i_app_core::{Kernel, TenantId};
use h5i_app_json::Value as Out;
use h5i_app_pgsql::{Column, Kind, Table};
use h5i_app_sql::{Val, Write};
use o5s_kernel as k;
use serde::Deserialize;

/// Marker type the traits hang off.
pub struct Calc;

impl Kernel for Calc {
    type Principal = k::Principal;
    type Snapshot = k::Snapshot;
    type Command = k::Command;
    type WriteSet = Option<k::Memory>;
    type Reply = k::Reply;
    type Error = k::Error;

    fn tenant(actor: &k::Principal) -> TenantId {
        TenantId(actor.org)
    }

    fn transition(
        actor: &k::Principal,
        snap: &k::Snapshot,
        cmd: &k::Command,
    ) -> Result<(Option<k::Memory>, k::Reply), k::Error> {
        k::transition(actor, snap, cmd)
    }

    fn apply(snap: &k::Snapshot, w: &Option<k::Memory>) -> k::Snapshot {
        k::apply(snap, w)
    }
}

pub fn principal(org: u64, user: u64) -> k::Principal {
    k::Principal { org, user }
}

fn col(name: &str) -> Column {
    Column {
        name: name.as_bytes().to_vec(),
        kind: Kind::Int,
        nullable: false,
    }
}

impl Store<Calc> for Calc {
    /// The `memories` table as `h5i_app_pg::table!` declares it for the
    /// kernel's `schema!`: columns in `to_row` order, `user` the key.
    fn tables() -> Vec<Table> {
        vec![Table {
            name: b"memories".to_vec(),
            columns: vec![col("user"), col("value")],
            key_len: k::Memory::KEY_LEN,
        }]
    }

    // Rows decode with the kernel's `decode`; writes go through `sql_writes`.
    fn load(rows: Vec<Vec<Vec<Val>>>) -> Option<k::Snapshot> {
        let mut rows = rows.into_iter();
        k::decode(&k::Rows {
            memories: rows.next()?,
        })
    }

    fn writes(w: &Option<k::Memory>) -> Vec<Write> {
        k::sql_writes(w)
    }
}

/// The JSON a client sends, e.g. `{"cmd":"apply","op":"mul","arg":7}`.
#[derive(Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case", deny_unknown_fields)]
enum CommandJson {
    Set { value: u64 },
    Apply { op: OpJson, arg: u64 },
    Get,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum OpJson {
    Add,
    Sub,
    Mul,
    Div,
}

impl From<CommandJson> for k::Command {
    fn from(c: CommandJson) -> Self {
        match c {
            CommandJson::Set { value } => k::Command::Set { value },
            CommandJson::Apply { op, arg } => {
                let op = match op {
                    OpJson::Add => k::Op::Add,
                    OpJson::Sub => k::Op::Sub,
                    OpJson::Mul => k::Op::Mul,
                    OpJson::Div => k::Op::Div,
                };
                k::Command::Apply { op, arg }
            }
            CommandJson::Get => k::Command::Get,
        }
    }
}

impl Api<Calc> for Calc {
    fn decode_command(body: &[u8]) -> Result<k::Command, String> {
        serde_json::from_slice::<CommandJson>(body)
            .map(Into::into)
            .map_err(|e| e.to_string())
    }

    fn encode_reply(r: &k::Reply) -> Out {
        match r {
            k::Reply::Value(v) => Out::obj([("value", (*v).into())]),
        }
    }

    fn encode_error(e: &k::Error) -> (StatusCode, Out) {
        let code = match e {
            k::Error::Overflow => "overflow",
            k::Error::Underflow => "underflow",
            k::Error::DivByZero => "division_by_zero",
        };
        (StatusCode::UNPROCESSABLE_ENTITY, crate::error_body(code))
    }
}

/// Lets clients retry with an `Idempotency-Key` header.
impl ReplyCodec<Calc> for Calc {
    fn fingerprint(cmd: &k::Command) -> Vec<u8> {
        format!("{cmd:?}").into_bytes()
    }

    fn scope(actor: &k::Principal) -> String {
        format!("u{}", actor.user)
    }

    fn encode(r: &k::Reply) -> Vec<u8> {
        match r {
            k::Reply::Value(v) => v.to_be_bytes().to_vec(),
        }
    }

    fn decode(b: &[u8]) -> Result<k::Reply, String> {
        let b: [u8; 8] = b
            .try_into()
            .map_err(|_| "stored reply is not 8 bytes".to_string())?;
        Ok(k::Reply::Value(u64::from_be_bytes(b)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `tables()` must agree with the kernel's `schema!`: table numbers, key
    /// length and column positions.
    #[test]
    fn tables_match_the_kernel() {
        let t = <Calc as Store<Calc>>::tables();
        assert!(h5i_app_pgsql::valid(&t));
        assert_eq!(k::Memory::TABLE, 0);
        assert_eq!(t[0].key_len, k::Memory::KEY_LEN);
        assert_eq!(
            t[0].columns[k::calc_tables::Memory::user as usize].name,
            b"user"
        );
        assert_eq!(
            t[0].columns[k::calc_tables::Memory::value as usize].name,
            b"value"
        );
        let m = k::Memory {
            user: 3,
            value: u64::MAX,
        };
        assert_eq!(k::Memory::from_row(&m.to_row()), Some(m));
    }
}
