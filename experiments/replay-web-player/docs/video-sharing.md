# Bridge video sharing integration

The native player build wires both entry points to the Engine v2 API. The
installed Bridge is not changed by compiling this workspace. Bundle the player
dist using the existing `tauri.player.json` overlay when preparing a test build.

## Replayer

The button posts `{replay_name, arena_unique_id}` to `/v1/share/authorize` from
the local player origin. Bridge validates the arena against the local scene,
creates ten-minute single-use state, and opens:

`https://engine.tfd.rocks/share/video/authorize?state=...&port=...&arena_unique_id=...`

The response includes `state`, `authorization_url`, and `opened`. The player
polls `/v1/share/status/{state}` while Engine handles login and confirmation.
No capability appears in this URL, browser storage, or local status response.

After confirmation, the Engine-origin page posts to `/v1/share/claim`:

```json
{
  "state": "the-original-state",
  "share_id": "Engine-share-UUID",
  "capability": "opaque-scoped-token",
  "capability_expires_at": "RFC3339-expiry",
  "arena_unique_id": "decimal-string",
  "max_bytes": 19922944
}
```

The claim is rejected unless state matches the pending local replay and arena.
State is single use; an expired or reused callback cannot start another job.

## Post-battle screen

From `https://engine.tfd.rocks`, POST the same payload without `state` to
`/v1/render/jobs`. Optional `replay_name` speeds lookup; otherwise Bridge searches
complete local replays newest first, matching the decoded arena ID. The lookup
runs outside the HTTP loop. Include the capability expiry returned by Engine;
when omitted, the native client uses a two-hour upper bound and Engine remains
the authority on actual token validity.

Both starts return `202 {id, state, status_url}`. Poll the returned `status_url`
(`/v1/share/status/{id}`), not the local render-job route: the sharing ID owns
authorization and delivery as well as rendering. Native progress includes the
underlying `job_id` once rendering starts. Repeated post-battle starts for the
same active ID attach to the existing operation.

States: authorizing, resolving, rendering, uploading, queued, posting, posted,
failed, expired. Rendering adds frame/total/attempt; posted can include the
Discord message URL. Credentials and local file paths are never public status.
Health advertises `replay-video-share-v1` only with a configured coordinator.
Engine POST preflights allow Content-Type and private-network access. Worker
mutation routes remain local-origin and separately authenticated.

## Native lifetime and testing

The coordinator validates the scoped capability against Engine before replay
lookup or rendering. The renderer verifies arena identity again. It saves MP4
locally, records actual frame-derived duration/dimensions, requests the ticket
after rendering, uploads with exact signed headers, then calls completion.
An uncertain completion is checked against Engine status before reporting a
failure. Encoded local output is retained on upload failure. Closing the tab
does not stop the native operation; quitting Bridge does. Jobs/tokens are
in-memory, so app restart requires authorization again.

Local checks cover callback reuse, state/battle binding, Engine-origin POST,
PNA/CORS, credential-free public status, native transport, and encoder behavior.
They do not exercise production storage or Discord delivery.

The Engine main tree inspected on 2026-09-06 contains the video API but no
matching authorization page path. That page and the Engine post-battle UI must
send the payloads above for end-to-end user testing. Discord posting and its
cleanup remain Engine responsibilities and were deliberately not triggered.
