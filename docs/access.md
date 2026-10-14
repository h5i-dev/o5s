# Access control, revocation and limits

## Who a request is

Every request carries `Authorization: Bearer v1.<organization>.<user>.<expiry>.<signature>`, an HMAC-SHA256 token signed with `H5I_APP_SECRET`. `o5s-server token ORG USER [SECONDS]` issues one that lasts a day, or SECONDS (`604800` for a week). The organization and user come only from the token, never from a request body. A repository belongs to one organization, and no other organization can see it (`Guarantees.isolation`).

## Repository roles

A repository starts **open**: every member of its organization can read, propose, review, report checks, merge and grant. This was the only mode before roles existed, so existing repositories keep working unchanged.

The first grant **restricts** it. From then on only the users granted a role have access:

| Role | Allows |
|---|---|
| `read` | Views, history, patches, documents, snapshots and Git export |
| `write` | Also proposals and channel forks |
| `review` | Also approvals, check reports and merges |
| `admin` | Also granting and revoking roles |
| `none` | Nothing; the repository is "not found" for this user |

```sh
o5s grant 7 review      # the first grant also makes you an administrator
o5s grant 7 none        # revoke
```

The API command is `{"cmd":"grant","repo":"…","user":7,"role":"review"}`. The registry kernel makes these decisions (`o5s_registry::authorize`, `grant`):

- A user without a role in a restricted repository gets "repository not found", as a user of another organization would.
- A user who can read but lacks the level gets "your role does not allow this".
- Only administrators grant, and nobody changes their own role. A restricted repository therefore always keeps an administrator.

Roles are rows of `o5s_roles`, committed through the same write-ahead log or PostgreSQL transaction as every other registry fact. Each command loads the whole role table of its repository, which is small. The repository directory still lists the names of restricted repositories to every member of the organization; it reveals the name, not the contents.

Independence of reviews is unchanged: an author still cannot approve or report checks on their own proposal, whatever their role.

## Revoking tokens

A token names no session, so revocation works at two granularities:

- **One token**: `POST /api/tokens/revoke` with `{}` revokes the token presented. A client that suspects a leak revokes its own token.
- **A user**: `POST /api/tokens/revoke` with `{"all":true}` revokes every token of the caller that expires no later than the one presented. An operator runs `o5s-server revoke ORG USER [NOT_AFTER]`, which by default covers every token `o5s-server token` has issued for a day; pass NOT_AFTER to cover longer-lived ones. `o5s-server token` then issues tokens that expire after the revocation.

Revoked tokens get `401 token revoked` on every route. The local backend keeps revocations in `revocations.json` in its data directory. The server rereads the file when it changes, so `o5s-server revoke` works while the server runs. PostgreSQL keeps them in `o5s_revoked_users` and `o5s_revoked_tokens`, shared by every replica and the Worker. A native server reuses a "not revoked" answer for up to 5 seconds. On Workers, Hyperdrive may cache the lookup for up to its query-cache lifetime, 60 seconds by default.

Rotating `H5I_APP_SECRET` revokes every token at once. Previous secrets in `H5I_APP_PREVIOUS_SECRETS` stay accepted until removed (`docs/storage.md`).

## Limits

| Variable | Limit | When exceeded |
|---|---|---|
| `O5S_MAX_REPOSITORIES` | Repositories per organization | `repository quota reached` on create |
| `O5S_MAX_PATCHES` | Patches per repository | `patch quota reached` on proposals |
| `O5S_COMMANDS_PER_MINUTE` | Commands (`/api/command`) per user per minute, with bursts up to a minute's worth | `429 too many commands` |

Unset means unlimited. Requests are also bounded by size: 4 MiB per request and per proposal, 1,024 files or edits per proposal, and 10,000 patches per admission batch. The native server counts commands per process. On Workers each isolate counts separately, so the command limit there is approximate.
