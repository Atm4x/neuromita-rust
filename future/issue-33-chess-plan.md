# Reserved: issue 33 — Chess

Status: reserved for implementation planning; research complete, implementation NOT started. This document supports an early draft PR to avoid overlapping work. No chess capability is claimed complete. Architecture and scope decisions below require discussion before implementation.

Issue: https://github.com/Atm4x/neuromita-rust/issues/33
Branch: codex/issue-33-chess-current
Base: upstream/main 15710db73adea787e9ddb23a3ea2550ded96d260
Integration dependency: PR68 / 0286b095 (canonical private Sea Battle state). Read-only inspection is allowed; do not merge until coordinated with parent.

## Research evidence

Issue33 was read live, including its complete functional scope and acceptance criteria. Upstream remote heads and all existing PRs were checked: no remote Chess/issue33 branch or Chess PR was found at that check. Recheck before publication.

Read docs/01-functional/desktop-mini-games.md and docs/01-functional/minigame-chess-seabattle-state-and-character-reactions.md. Actual Python reference is C:/Development/Atm4x_NeuroMita at 36f942106ab04522492477087e7db5ca41f0d626; the inspected Chess/GameManager/GameInterface files had no local changes. Sources: modules/Chess/{board_logic,game_instance,engine_handler,chess_board}.py, modules/{game_interface,session_snapshot}.py, managers/game_manager.py, mini_game_session_registry.py, prompt_controller.py and chess reaction tests.

Confirmed behavior:
- Start/end/restart, player color, easy/medium/hard mapped to Maia1100/1500/1900, legal moves and explicit pawn promotion.
- FEN, UCI/SAN, history, check/mate/stalemate, insufficient material and move-count/repetition draws. Python treats a claimable draw as game over; preserving or changing that policy must be explicit.
- RequestBestChessMove, MakeChessMoveAsLLM, ChangeChessDifficulty, EnableAuto/DisableAuto. Auto engine and automatic in-character reactions are separate controls; manual reaction, close and game-over reactions are active.
- Cheats are actively gated: EnableCheats/DisableCheats, CheatMove, CheatSpawn, CheatRemove. They require a separately agreed canonical edit/validity contract, not an unchecked normal-move bypass hidden in the legal API.
- Lc0 is an external UCI executable; Maia weights are external assets. python-chess supplies rules. Gameplay readiness and engine readiness are separate.
- Current Python stores root FEN + UCI history + resulting FEN and exact paused snapshots. The async engine-result path checks generation, engine identity and originating position snapshot; the older audit's missing-fence observation does not describe this newer path. Some auto paths still call the engine synchronously.
- The owner sees its board/legal moves; other characters receive only public session summary. Game identity is bound to its original character.

## Proposed architecture (not yet implemented)

Extend the reviewed PR68 game domain and one-active-game-per-character rule. Add a typed Chess state/command variant rather than a second game registry. Canonical authority is Timeline: private GameStateCaptured contains validated session identity/revision, root position, accepted move history, color, modes, difficulty and outcome; sanitized public facts are committed with it. CharacterHandler owns only reconstructed working copies.

Historical reads use the requested prefix. Fork inherits only the cut-off state, with branch-local subsequent play. SQLite/Core restart reconstructs history and draw state. FEN alone is insufficient for repetition. Read projections never publish effects or start engine work.

User/model actions validate session, branch/selection epoch, canonical state token, actor role, turn and legality. Model response plus game changes and other accepted effects commit atomically before Finished. Any invalid/stale action rejects the whole proposed batch. User operations have stable IDs for uncertain retry.

The UCI executor owns its process and bounded search mailbox. It accepts an immutable position with a session/revision/selection ticket, runs outside Timeline locks, and returns a proposal. The owner revalidates ticket and move legality before commit. Restart/close/fork/difficulty changes invalidate outstanding tickets. Bound handshake/search/output/shutdown; kill/reap only the owned process. Runtime tasks and process handles are not canonical game state. After restore, any resumed work receives fresh runtime generation and cannot accept a pre-restart result.

Reactions originate from committed facts and use existing admission. Rejection of a reaction does not roll back an accepted player move. Owner/private and social/public projections remain separate; no engine paths or private board state in shared summaries.

User-facing acceptance requires an actual playable surface (typed presenter/binding and an agreed CLI/desktop board), not just domain structs or a mock-only engine port. The exact surface is a planning decision; Qt implementation details are not required.

## Dependency and offline vendor proposal

Preferred candidate pending agreement: cozy-chess =0.3.4, MIT, with cozy-chess-types0.2.2 (MIT, no further normal dependencies). It provides legal generation, FEN and SAN/UCI conversion. Sources:
- https://github.com/analog-hors/cozy-chess/blob/master/cozy-chess/Cargo.toml
- https://github.com/analog-hors/cozy-chess/blob/master/types/Cargo.toml
- https://docs.rs/cozy-chess/0.3.4/cozy_chess/util/index.html
- https://docs.rs/cozy-chess/0.3.4/cozy_chess/struct.Board.html

Important gaps: Board retains no history; status does not detect repetition/insufficient material. Its halfmove clock caps at100 and status applies the50-move draw. The game domain must preserve exact counters/history and implement the agreed draw policy; library status alone cannot be the final verdict. Standard UCI castling must use util conversions rather than internal king-captures-rook notation. Verify selected released crate manifests/checksums and actual Rust1.89 compatibility before adoption.

Alternative: shakmaty has broader rule/notation support but GPL-3.0-or-later; current upstream workspace declares Rust1.97 while this project declares1.89. Select a compatible release and licensing policy explicitly if choosing it instead.

No chess crate was present in the inspected lock/vendor or local crate cache. Adding the dependency requires intentional Cargo resolution/vendor generation, inspecting only new packages and lock delta, preserving all existing vendor bytes/checksums/licenses, and proving subsequent --locked --offline operation. Do not hand-edit third-party sources.

Lc0 executable and Maia weights are runtime assets, outside Cargo vendor. Configure paths/readiness explicitly; do not silently download or substitute engine strength. Missing engine must yield an actionable unavailable result while legal manual/model moves remain usable. Asset distribution/licensing and installation scope must be recorded separately. No live model/inference/download was performed during research.

## Acceptance matrix

- Rules/notation: valid/invalid FEN; SAN ambiguity/check/mate; UCI castling; pins/check; en passant including discovered check; all four promotions and missing promotion choice; mate/stalemate/material/count/repetition outcomes.
- Draw parity: current and next-move claimability,50/75-move counters,3/5-fold repetition, castling/en-passant rights in repetition identity. Results remain identical after snapshot/reopen/fork.
- Lifecycle: start, close versus resign, new identity on restart, terminal board retained for its result reaction, settings gates, multiple owners and one active game per owner.
- Canonical storage: actual historical reads during active play, fork cut-off/sibling divergence, SQLite restart, validation of corrupted or inconsistent history, no private projection leakage.
- Atomicity: valid and invalid multi-action batches; simultaneous player/model/engine moves; no text/effects committed on stale/illegal proposal; cancellation before acceptance.
- Engine: real owned UCI adapter with a controlled local process fixture; handshake/option/search, promotion, late result after restart/close/fork/config change, hung/crashed/noisy engine, queue full and bounded cancellation/shutdown. A configured real Lc0/Maia smoke is a separate evidence level.
- Reactions: auto engine versus auto reactions, manual Mita turn, checkmate/close/restart result instructions, admission failure without losing accepted gameplay.
- Independent default/no-default offline tests, Clippy --all-targets -Dwarnings and fmt; exact counts and proof boundaries reported. Cargo slot must be reserved with parent and held through test execution, using the agreed shared target/debug0/incremental0/vendor path.

## Decisions before code

1. Rules dependency: cozy-chess with explicit domain history/draw layer, or another reviewed library.
2. Preserve Python auto-end on claimable draws or require explicit claim.
3. Cheats: separate gated canonical-edit scope, including invalid-position handling; do not silently omit active functions while claiming full parity.
4. Playable surface and Lc0/Maia asset provisioning scope.

No implementation, dependency resolution, Cargo runs, merge or publication has been performed for issue33 at this checkpoint.
