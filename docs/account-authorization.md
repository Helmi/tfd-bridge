# Bridge account authorization — 0.19.0: agreed Engine contract

Status: **contract agreed and frozen (v5)** on 2026-09-18. Implementation remains
queued; **no Engine code exists for this contract yet** and nothing here is a
running service. Current Engine implementation is **not ready until proven** by
its deployment compatibility gate (Engine td-66b585).

- Normative source: `private-sync/notes/engine-bridge-auth-contract-v5.md`
  (gitignored, Syncthing-synced). Frozen SHA256
  `cdf07dd68bb9911a62f54440bcb42b08295a389796a172a2cce83d9b8979e880`.
- This file is the public record of the agreement: operation schemas, decisions,
  validation sequences and Engine task links. It references the frozen contract
  ("§n") instead of duplicating it byte for byte.
- History: v3 was frozen and Bridge-approved for sections 1–8 and 11 (SHA256
  `f362aacc7ab2c6fe5a5254e8e7de07d02a58fa97962cf3ed9e335723ec221089`); the
  Engine verified the identical v3 hash. v4/v5 changed only the replay-ownership
  sections to apply Frank's owner decisions of 2026-09-18. v5 supersedes v1–v3.
  Coordination thread: Dropbox `tfd-bridge-019-account-auth`.

Epic: **td-b48730 — 0.19.0: shared Engine authorization and own-replay sharing**.

## Problem and outcome

Video sharing currently initiates a separate authorization flow for each share.
The initial native call uses the OS browser opener, but the previous player UI
ignored opening failures and offered a normal webview link as fallback. The
cause of the reported installed-build failure has not been reproduced. A missing
Engine authorization page was reported on September 6; that note is historical.
Page availability is proven by the Engine deployment gate (td-66b585), not
assumed — no Engine auth code exists today.

For 0.19.0, the user connects Bridge to Engine once. The native client maintains
that connection, embedded Engine views use the same identity, and the user can
share their own replay without logging in again. Signing in does not itself
authorize a publication: destination and sharing confirmation remain explicit.

## Fixed requirements

- Version **0.19.0**, including migration for existing users.
- Mandatory account connection: no Later or skip option.
- Authorization in the OS default browser, independent of profile-link settings.
- Silent regular renewal; after three staggered transient attempts fail, show a
  renewal-failed message and recovery action. Definitively revoked credentials
  require reauthentication instead of ineffective retries.
- Dashboard, Engine, Monitor and sharing use the same renewable authorization.
- Sharing is limited to the logged-in user's recorded replay. Other-player
  background replay archiving remains permitted.
- Local debug build provides **Log out and reset authorization** for retesting.

## Scope and ownership

Bridge owns native browser launch, callback handling, secure credential storage,
renewal scheduling, account UI and gates, embedded-session handoff consumption,
local replay checks, and debug reset. Engine owns every authorization/session
endpoint, linked game identity, authoritative share ownership checks, revocation
and publication. There is no Bridge backend to introduce.

Do not redesign replay rendering, donation/archive consent, game-folder setup,
normal profile-link preferences or release infrastructure as part of this work.
Preserve existing settings during account migration. Publishing a release is a
separate action after the verification gate.

## User experience

First interactive launch after install or the introducing update requires
"Connect to TFD". There is no Later/skip option. Authorization opens in the OS
default browser regardless of the normal Engine-link preference. Autostart never
opens a browser; the Engine never requires interactive login to keep a refresh
alive (§0, §8).

One native account connection authenticates Dashboard, Engine, Monitor and the
replayer. Renew credentials silently before expiry. Transient renewal failures
retry three times total, staggered (5 s then 30 s, honouring a longer
`Retry-After`), then display an explicit renewal failure and a recovery action.
Revoked credentials require sign-in again. Expired credentials cannot authorize
protected operations, even while a renewal retry is pending.

The connection screen shows opening/waiting, cancellation, expiry and failure
states with a clear retry action. The connected dashboard identifies the active
account. Recovery returns to the same connection flow rather than presenting an
independent Engine login. Startup, tray navigation, Monitor and replayer routes
must all honor the account gate; merely hiding buttons is insufficient.

## Agreed contract v5 — normative summary

All schemas below are from frozen v5. Values in examples are synthetic
(`EXAMPLE`, `dev_example`, `100000001`); the frozen contract file remains the
byte-level authority.

### Trust model

The Bridge is a **public client**: no shipped secret is trusted. Security rests
on possession binding (DPoP over an EC P-256 key in the OS credential store),
single-use refresh with reuse detection, and server-derived authority. DPoP does
not prove an official binary, stop same-user malware, or stop a user driving the
API with their own key. It does stop a copied token working elsewhere, turn
silent token copying into a visible revocation event, and make one revoke call
kill tokens, webview sessions and in-flight share work (§1). Authenticode
signing, a non-exportable CNG/TPM key and attestation are separate later tasks;
none blocks 0.19.0 and none may be claimed in docs before existing.

### Credential lifetimes (§2)

| Credential | Format | Lifetime | Storage |
| --- | --- | --- | --- |
| Device key | EC P-256 (ES256) | until logout/reset | OS credential store |
| Authorization request | server-side record | 300 s | Engine (Redis) |
| Authorization code | 43-char opaque | 60 s, single use | Engine memory |
| Access token | JWT ES256 DPoP-bound (`cnf.jkt`) | 900 s | memory only |
| Refresh token | 256-bit opaque, SHA-256 hashed at rest | 90 d sliding, 365 d absolute | OS credential store |
| Handoff ticket | 256-bit opaque | 60 s, single use | Engine memory |
| Web session cookie | existing `tfd_session` JWT | leased to current access-token expiry (§7) | webview cookie jar |
| Share capability | existing opaque Redis capability | 2 h | Engine memory |

Client timeouts: HTTP connect 10 s, read 30 s; `/token` server processing under
2 s; DPoP skew ±60 s; `jti` replay cache 300 s; idempotency cache 120 s. The
365-day absolute cap is the only forced re-login (vacation guarantee, §2).

### Operations

Base `https://engine.tfd.rocks/api/v1/bridge/auth`. Native calls to `/device`,
`/authorize`, `/token`, `/revoke`, `/me`, `/handoff` must send
`X-TFD-Bridge-Version` and `User-Agent: TFD-Bridge/<semver> (<OS>)`; missing or
unparseable: `400 bridge_version_required`. Browser-driven navigation
(`authorization_url`, WG login redirects, loopback callback, `handoff_url`) is
exempt and is bound by `request_id`, `state`, PKCE and single-use tickets
instead (§4). Every error body is
`{"error":"...","error_description":"...","request_id":"..."}`.

| Operation | Method & path | Auth | Request | Response | Expiry | Key errors |
| --- | --- | --- | --- | --- | --- | --- |
| Metadata | `GET /metadata` | none | — | endpoints, alg/campaign support, lifetimes, `min_bridge_version`, `realm_required_for_sharing` | cacheable discovery | — |
| Device register | `POST /device` | DPoP | public JWK, `device_name`, `platform`, `bridge_version`, `install_id`, `client_profile` (`release` / `debug`) | `201/200` `device_id`, `registered_at`, `client_profile`; idempotent on same JWK+`install_id` | device key lives until logout/reset | `invalid_dpop_proof`, `409 device_conflict` |
| Authorize | `POST /authorize` | DPoP | `device_id`, PKCE `code_challenge` (S256), `redirect_uri` (loopback, see below), `state`, `scope` | `authorization_url`, `request_id`, `expires_in: 300` | request 300 s | `invalid_redirect_uri` |
| Token exchange / refresh | `POST /token` | DPoP | grant A `authorization_code`+`code_verifier`, or grant B `refresh_token`; both with `device_id` | access+refresh pair, absolute expiry timestamps, `web_lease_expires_at`, scope, account object | access 900 s; refresh 90 d/365 d | `invalid_grant` (+`reason`), DPoP family, `429 rate_limited` + `Retry-After`, `503 auth_backend_unavailable` + `Retry-After` |
| Revoke | `POST /revoke` | DPoP (+ `Authorization: DPoP <token>` where required) | `device_id`, optional `refresh_token`, `scope: device / all_devices`, `reason` | `revoked`, counts, `share_jobs_cancelled`/`share_jobs_uncancellable`, `client_profile`, `revoked_at` | idempotent on already-revoked | `401 invalid_token`, `403 insufficient_scope`, `400 invalid_request` (`refresh_device_mismatch`) |
| Current account | `GET /me` | `Authorization: DPoP <access_token>` + DPoP | — | account object, `permissions`, `device_id`, `client_profile`, expiry echoes, `devices` list | echoes only, extends nothing | DPoP family, `403 device_revoked` |
| Web handoff | `POST /handoff` | `Authorization: DPoP <access_token>` + DPoP | `device_id`, `target: dashboard / monitor / web`, `return_path` (same-origin allowlist) | `handoff_url`, `expires_in: 60`, `web_lease_expires_at` | ticket 60 s, single use; lease = access-token `exp` | `400 invalid_return_path`, `410 ticket_consumed`, consume-time `401/403` revalidation |
| Share video (existing endpoint, extended) | `POST /api/discord/share/video` | authenticated session + existing share capability | existing body + `recorder_account_id` (required), `recorder_realm` (optional) | existing share response | capability 2 h | `403 recorder_account_mismatch`, `403 not_battle_owner`, `403 recorder_realm_mismatch`, `409 capability_revoked` |

Redirects and origins (§4.3, §4.7): `redirect_uri` must be `http` on host
exactly `127.0.0.1`, port `43210..43214`; the path is the Bridge's choice and is
echoed unchanged. No custom URI scheme. The OS browser opens
`authorization_url`; outcomes arrive as loopback query parameters carrying the
original `state`: success `?code=...&state=...`, `error=access_denied`,
`error=account_not_authorized` (terminal), `error=expired_request`; a closed
browser delivers nothing and the Bridge times out after `expires_in`. `state` is
compared in constant time; unknown states are ignored. `handoff_url` is opened
in a webview; the Engine atomically consumes the ticket and revalidates device,
refresh chain, lease and token revocation at consume time before setting the
`tfd_session` cookie and 302-ing to `return_path` (allowlist prefixes `/next/`,
`/dashboard`, `/monitor`, `/battles`, `/profile`, `/settings`; no open-redirect
surface by construction).

Synthetic examples (all identifiers invented; §4 has the exact schemas):

```json
// POST /token — grant A (authorization_code)
{"grant_type":"authorization_code","code":"EXAMPLE_CODE","code_verifier":"EXAMPLE_VERIFIER",
 "device_id":"dev_example_001","redirect_uri":"http://127.0.0.1:43210/auth/callback"}
// 200
{"access_token":"<jwt>","token_type":"DPoP","expires_in":900,
 "access_token_expires_at":"2030-01-01T12:15:00Z","web_lease_expires_at":"2030-01-01T12:15:00Z",
 "refresh_token":"<opaque>","refresh_expires_in":7776000,
 "refresh_expires_at":"2030-03-31T12:00:00Z","refresh_absolute_expires_at":"2031-01-01T12:00:00Z",
 "scope":"bridge.submit bridge.share bridge.profile",
 "account":{"user_id":1001,"account_id":100000001,"realm":"eu","nickname":"ExampleUser",
            "clan_tag":"EXA","role":"member","linked_account_verified":true}}
```

```json
// POST /api/discord/share/video — new evidence fields (§9.2)
{"arena_unique_id":1234567890,"entry_point":"post_battle","custom_message":"",
 "recorder_account_id":100000001,"recorder_realm":"eu"}
```

Refresh rotation is one state machine (§5): `ACTIVE` → `ROTATED_UNCONFIRMED` on
use; a re-presentation of an unconfirmed node re-rotates with fresh material and
orphans the successor (lost-response recovery); presenting a `SUPERSEDED` node
is theft and revokes the whole family. Confirmation happens when the successor
refresh is presented or an access token carrying its `rtc` claim is accepted on
any protected endpoint; it is a single synchronous PostgreSQL transaction under
the per-device row lock, and a failed confirmation aborts the whole request with
`503 auth_backend_unavailable` and no side effect. Transport retry reuses the
same DPoP proof/`jti` within the 120 s idempotency cache; a retry proof must be
under 60 s old when sent, otherwise a fresh proof is minted. DPoP failures never
revoke. `/token` rate limits: 10/min and 60/hour per device → `429` +
`Retry-After`; `Retry-After` never exceeds 300 s on `/token`. Store failures on
either side strand nothing: the predecessor stays usable for the full chain
lifetime (§5.7, §5.8).

Web sessions: exactly one per device; every handoff mints the same session, so
webviews cannot evict each other. The lease equals the minting access token's
`exp` and is extended only by a successful `/token` renewal; the Engine refuses
past-lease sessions with `401 web_lease_expired` and the Bridge gates locally on
the echoed timestamps (§7). Local cookie clearing proves nothing; the server row
is the authority (§7.3).

Reset ordering is fixed (§8): **revoke first** (`reason=debug_reset`, 3 s
connect / 5 s read, 2 attempts, 15 s budget), record `revoked_remote` or
`revoke_failed`, wipe unconditionally (credentials of that `client_profile`, all
embedded cookies/storage, awaiting verified completion), preserve game settings,
replays, archive consent and the system-browser login, and report honestly — on
`revoke_failed`: "signed out locally, server revoke pending", no durable retry.
Engine-side recovery: the device stays revocable from any other session via
`/me.devices` and is superseded by the next completed login from the same
`install_id`.

Full error catalogue: §11. Realm handling and share evidence tiers: §9–§10 and
below. Engine-side work implied: §12.

## Resolved decisions

Everything below was previously open and is now fixed by v5. Nothing in the
contract is pending an owner decision (§13).

| Decision | Resolution | Contract |
| --- | --- | --- |
| Own-replay sharing | Decided by Frank 2026-09-18: users share their own replays. Two **independent** conditions, both required: (1) recorder binding — share request carries `recorder_account_id` read from the replay; must equal the authenticated session account, else `403 recorder_account_mismatch`; (2) owner row — a stored player row for that `arena_unique_id` has `is_battle_owner` true for the authenticated account, else `403 not_battle_owner`. The client claim can only narrow access; the owner row is server-derived. | §9, §9.1 |
| Realm | Optional and narrowing only. `recorder_realm` normalized to `eu`/`na`/`asia`/`cis`; must equal `users.realm` when present, else `403 recorder_realm_mismatch`; **absent is permitted**. The earlier "unknown realm fails closed" rule is withdrawn. Engine derives realm from the EU-only WG login and sets/backfills `users.realm = eu`. | §9.2, §10 |
| Server replay verification | Separate concern. Ownership evidence is tiered and labelled: Tier 0 client-asserted/session-anchored (`verified: false`, provenance not proof), Tier 1 corroborated, Tier 2 server-verified — out of scope for 0.19.0 and tracked as Engine **td-349ab4**. Not a precondition of the sharing rule. | §9 |
| Auth feature switch | **None.** Owner decision 2026-09-18: no deployment-time switch for this surface; if the Engine is deployed, the rule is live. Existing `system_settings` break-glass flags are not extended to auth. | §9 |
| Unlinked accounts: onboarding vs sharing | Onboarding **is** the linking. Mandatory interactive login with no skip (§0); the WG login mints an account with `linked_account_verified: true` (§4.4). A WG login that succeeds but lacks TFD access (guest, not in a TFD clan) is refused terminally at the callback with `account_not_authorized`; retrying will not help (§4.3). There is therefore no connected-but-unlinked mode to define. Sharing is not gated at onboarding: a fully linked user simply cannot share a battle where the two §9.1 conditions fail. | §0, §4.3, §4.4, §9.1 |
| Multiple linked game accounts | One account per device family. Logging in as a different account on the same install revokes the previous family, its web sessions, handoff tickets and queued share grants (`account_changed`, §6). Multiple devices per user are supported; `all_devices` revoke covers them within one `client_profile` (§4.5). | §4.5, §6 |
| Offline startup / autostart / archiving | Connect is mandatory only on interactive launch; autostart never opens a browser. Offline, local work and archiving continue; protected work stops once the access token expires. A 90-day refresh means reconnecting never needs interactive login. Webviews work only to the current lease. | §0, §7.2, §8 |
| Debug vs release credentials | Separate namespaces via `client_profile`. A debug reset can never revoke release credentials and the reverse is forbidden, enforced server-side. | §4.2, §4.5 |
| Loopback callback | Scheme `http`, host `127.0.0.1`, port 43210–43214; path free; no custom scheme. Same port set as the bridge server discovery. | §4.3 |
| Version headers | Mandatory on the six native endpoints only; browser navigation exempt. | §4 |
| `/device` safety | Registration never revokes anything; supersession of an old device requires a completed login by the same owner+profile or a revoke proof by the old key. | §4.2 |
| Webview revocation & job cancellation | Revoke returns `web_sessions_revoked`, `share_jobs_cancelled`, `share_jobs_uncancellable`; account change does the same implicitly; handoff tickets are revalidated at consume time. | §4.5, §4.7, §6, §7 |
| Realm source (Bridge) | The toolkit decoder exposes per-player `realm` (`crates/wows-replays/src/analyzer/decoder/decode.rs`); Bridge sends it as a claim only. | §10 |

## Validation sequences (td-5442ce)

Each sequence required by this task is covered by frozen v5. None has been run
against a live Engine — no Engine code exists. Examples are synthetic; no
secrets.

| Sequence | v5 coverage | Contract behavior |
| --- | --- | --- |
| Success sign-in | §4.2–§4.4 | `/device` → `/authorize` (PKCE S256, `state`) → OS browser → loopback `?code=&state=` → `/token` grant A returns DPoP-bound access (900 s) + rotating refresh; example above. |
| Denied / expired callback | §4.3 | `access_denied` (user cancelled), `expired_request`, terminal `account_not_authorized` (no TFD access); closed browser → local timeout after `expires_in: 300`; unknown `state` ignored; `state` compared constant-time. |
| Refresh reuse / lost response | §5.1–§5.5 | Reused `SUPERSEDED` node → family revoke, `invalid_grant` `refresh_reuse_detected`. Lost response → predecessor `ROTATED_UNCONFIRMED` re-rotates fresh material for the full chain lifetime. Transport retry reuses `jti` for an idempotent 120 s replay; cached entries are validated (fingerprint + live family) and can never resurrect a revoked pair. |
| Revoked token | §4.5, §5.6, §7.1 | `/revoke` kills device, web sessions and cancels share jobs; `all_devices` needs a valid `bridge.profile` access token and never crosses `client_profile`; DPoP failures and rate limiting never revoke; past-lease webviews get `401 web_lease_expired`. |
| Account switch | §6 | Previous family, web sessions, handoff tickets and queued share grants revoked; later use of old credentials → `invalid_grant` `account_changed`. |
| Missing linked identity | §4.3, §9.1 | Cannot occur in a connected state: onboarding login is the linking, and a WG login without TFD access terminates with `account_not_authorized`. At share time, no owner row for the account → `403 not_battle_owner`. |
| Forged ownership | §9, §9.1, §9.2 | Client-asserted `recorder_account_id`/`recorder_realm` can only narrow; the Engine derives the owner row server-side and ignores client `is_battle_owner`. Distinct refusals (`recorder_account_mismatch`, `not_battle_owner`, `recorder_realm_mismatch`); evidence level stated (`verified: false` at Tier 0). |

A Bridge renewal failure and recovery drill, secure-store write failure drill
and all live Engine rounds are downstream validation for td-27bcc9/td-cab09a and
the end-to-end task td-853c9b, against a deployed Engine that passes td-66b585.


### Bridge local replay preflight (client-only)

`POST /v1/share/preflight` is available only to `https://engine.tfd.rocks`.
It accepts an explicit `{ "replay_name", "arena_unique_id" }` and returns
`{ "preflight_id", "recorder_account_id", "recorder_realm"? }`. The account ID
is always a decimal string. `preflight_id` is opaque, short-lived and single-use.
It binds the canonical replay path, source SHA-256, arena and account lifecycle
locally. It is not a server ownership assertion; Engine must still enforce v5 §9.1.

**External blocker:** the Bridge must not substitute a local owner decision or call an
Engine owner-row endpoint. Sharing remains blocked for live use until Engine deploys
`POST /api/discord/share/video` with the v5 `recorder_account_id` / optional
`recorder_realm` checks and its server-derived `is_battle_owner` row gate (Engine
`td-66b585` deployment proof).

## Engine implementation status and compatibility gate

Engine epic **td-8274fe** (created only after agreement). Child tasks:

| Engine task | Area |
| --- | --- |
| td-425628 | schema |
| td-18f0d5 | DPoP |
| td-ffe0f8 | metadata + device |
| td-60b956 | authorize |
| td-d6aa8f | token |
| td-886b03 | revoke |
| td-c5a26d | me |
| td-41d6a6 | handoff / web lease |
| td-e2c4b4 | protected endpoints |
| td-728b6e | realm (`users.realm`) |
| td-66b585 | deployment compatibility gate |
| td-dc56bd | Engine-side docs |
| td-954091 | sharing restriction (owner decision now made) |
| td-349ab4 | server-side replay verification (Tier 2, post-0.19.0) |

Compatibility gate:

- Wire-level: `/metadata` exposes `min_bridge_version` (`0.19.0` in the v5
  example) and the Engine answers stale native clients with
  `426 upgrade_required` (§4.1, §11).
- Deployment: Engine td-66b585 must pass before any Bridge code claims working
  authorization against Engine. **No Engine code exists as of v5; the current
  Engine implementation is not ready until proven.**
- Enablement: there is no deployment-time auth feature switch (owner decision);
  existing routes stay compatible and Bridge tasks do not wait on a flag.

## Native foundations implemented

`bridge-core::account` is a credential-free renewal state machine. It prevents
concurrent refresh requests, invalidates stale responses after logout/account
changes, and gates account operations at expiry or exhausted retries. Retry
delays are 5 and 30 seconds after the preceding failure, honouring longer server
`Retry-After` values — these now match contract v5 (§0, §5.5) instead of being
local guesses. Access/refresh lifetimes are contract values (§2), not invented
locally.

The state machine is not yet connected to the app, a credential store, timers,
network requests, onboarding UI or embedded sessions. Its native caller must
serialize response acceptance and durable credential rotation under the same
lock, with bounded HTTP timeouts. Lost refresh responses and secure-store write
failures recover via the predecessor-stays-usable rule (§5.1, §5.3, §5.8); the
recommended two-slot `current`/`pending` store shape is in §5.8.

The existing video authorization fallback now retries through a local native
browser-opening route. The route accepts only the current pending state and
reconstructs the known Engine URL. It accepts neither a caller-supplied URL nor
an Engine-origin popup request. Failed initial browser opening is visible.

## Still open (explicit, not blocking contract agreement)

- **Bridge-side verification:** confirm recorder-level realm extraction from
  real replays and the normalized value set (`eu`/`na`/`asia`/`cis`) on the
  Windows rig (§10 open item). Until then the field is a claim that may be
  absent; absence is permitted by the contract.
- **Engine implementation:** none exists; td-8274fe epic and td-66b585 gate must
  land before live validation.
- **Authenticode signing:** separate task, not blocking 0.19.0 (§1).
- **CNG/TPM non-exportable key:** reachability not investigated by either side
  (§1); 0.19.0 assumes a software key and upgrading changes no wire format.
- **Tier 2 server replay verification:** Engine td-349ab4, out of 0.19.0 scope.

## Contract request history (fulfilled by v5)

The request below was sent to the registered Engine agent on the Dropbox
channel, thread `tfd-bridge-019-account-auth`, before implementation. It is
preserved as history; every bullet is now answered by frozen v5.

- Browser authorization with code, PKCE and state; exact callback and exchange
  schema, expiry, cancellation and replay protection. → §4.3, §4.4, §5.
- Short-lived access tokens and rotating refresh credentials, including TTLs,
  refresh replay/lost-response handling, error codes and device revocation.
  → §2, §4.4, §4.5, §5.
- Current account with verified linked game account ID and realm. → §4.6, §10.
- Single-use embedded-session handoff under the same renewable authorization and
  revocation lifecycle; native refresh never exposed to page JavaScript, URLs,
  logs or public loopback status. → §4.7, §7.
- Sharing ownership evidence and endpoint changes; existing per-job capabilities
  scope a confirmed sharing operation. → §9.

All server implementation belongs in tfd-engine. Bridge remains a client.

## Debug logout and reset

Expose a clearly labeled local-debug action that uses the common logout path and
removes all Bridge authorization state needed to reproduce first login:

- Native access/refresh credentials, including the persistent OS-store entry,
  scoped to the `client_profile` namespace.
- Pending authorization callbacks, handoffs and renewal requests.
- Embedded Engine cookies and login storage, across all Engine views, awaiting
  verified completion of the asynchronous clear.
- Account-bound sharing jobs/capabilities per the revocation contract (§4.5).

Agreed order (§8): revoke first while credentials still exist
(`reason=debug_reset`, 15 s total budget), record `revoked_remote` or
`revoke_failed`, then wipe unconditionally and wait for real cleanup. Report
honestly on partial failure — "signed out locally, server revoke pending" — with
no durable retry promised. Engine-side recovery keeps the device revocable from
any other session and superseded by the next completed login from the same
`install_id`. Do not erase replay files, game-folder configuration or archive
consent, and do not clear the user's system-browser login. Successful reset
means the mandatory connection screen returns and a restart stays signed out;
repeated reset is safe. The debug action must not be exposed in release builds.

## Task breakdown and dependencies

Bridge tasks are children of td-b48730. Engine backend work is cross-linked via
epic td-8274fe (table above).

| Task | Deliverable | Depends on |
| --- | --- | --- |
| td-5442ce | Agree Engine contracts — done by frozen v5 | Engine agreement (v5) |
| td-27bcc9 | Native browser sign-in and secure credential storage | td-5442ce |
| td-cab09a | Silent token rotation and bounded renewal recovery | td-27bcc9 |
| td-980190 | Mandatory install/update account onboarding | td-27bcc9, td-cab09a |
| td-9994bd | Authenticate all embedded Engine views | td-27bcc9, td-cab09a |
| td-9c8467 | Restrict sharing to the connected recording player | td-5442ce, td-27bcc9, td-cab09a |
| td-96a620 | Debug logout and complete local authorization reset | Native, renewal, onboarding, embedded views, sharing |
| td-853c9b | End-to-end Windows and upgrade verification | Onboarding, embedded views, sharing, debug reset, Engine deployment gate |

Each task carries its own Goal, Scope, Technical constraints, Risk notes,
Done-state and Validation. Existing foundation changes need review as part of
the native/renewal tasks; they are not grounds to close those tasks.

## Release gate

Version metadata is 0.19.0, but this is not a release-ready build. Finish the
agreed transport, OS credential storage, mandatory onboarding, webview session
handoff/renewal and ownership enforcement first, against an Engine deployment
that passes td-66b585. Verify install/update/restart, default-browser opening,
silent refresh, exhausted retries, revoked credentials, account switching, all
embedded views and real sharing on Windows. Unit tests and player builds do not
prove production authorization or Discord delivery.

Current foundation checks: bridge-core **195 passed, 4 ignored**; Bridge player
production build passed; native `cargo check -p tfd-bridge --offline` passed.
No end-to-end account login, OS credential persistence, embedded session renewal,
ownership enforcement or debug reset has been verified. No install or release
has been performed for 0.19.0.
