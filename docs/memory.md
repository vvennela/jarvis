# Memory and resource discipline

Rust gives explicit ownership and bounded lifetimes, but it does not automatically bound queues, database growth or provider payloads. The prototype uses the following implemented limits. These are application bounds, not a measured total-process RSS guarantee.

## Implemented limits

| Area | Current bound | Behavior at the boundary |
|---|---|---|
| Per-resource content | 64 KiB UTF-8 | Reject oversized observations; skip oversized files |
| Resource inventory | 2,048 resource IDs by default, including tombstones | New resources rejected at capacity; existing resources can update |
| Event history | 4,096 metadata-only events by default | Retain newest events; no full historical content copies |
| Action ledger | 128 pending entries by default | Reject additions at capacity; expire/consume candidates |
| Library query | 1,024 bytes, first 16 whitespace-delimited terms, max 32 results | Escape terms as literal FTS phrases; reject invalid lengths/counts |
| CLI context packet | Up to 4 excerpts of 4 KiB each | UTF-8-safe truncation with explicit marker |
| Filesystem scan | 4,096 entries, 1,024 supported files, depth 8 | Report truncation; avoid deletion inference from incomplete scan |
| Watch queues | 128 event slots and 128 dirty paths | Coalesce/reconcile on overflow; no unbounded event backlog |
| File cache | At most 1,024 metadata fingerprints | Avoid content rereads when identity/size/timestamps are unchanged |
| Provider text input | 64 KiB combined system/user input | Reject locally before network request |
| Provider response | 1 MiB wire body; 64 KiB returned text | Check declared and streamed body size; fail explicitly |
| Output tokens | At most 4,096 per call; `ask` uses 1,024 | Reject larger caller values |
| Provider concurrency | 2 active requests per client | Queue wait counts toward total deadline; borrowed text avoids another queued copy |
| Provider deadline | Jev 10 seconds; Crusoe 60 seconds | Includes queue, request and response body; dropping the future stops local waiting |
| Native bridge frame | 512 KiB | Reject before allocating oversized body |
| Browser metadata | URL 8,192 bytes; title 2,048; IDs 128 | Validate before any evidence write |
| Extension buffers | 32 queued tabs; 256 cached tab/document entries | Latest-state coalescing and bounded eviction |
| Local `.env` | 16 KiB | Reject oversized or invalid UTF-8 configuration |
| Conversation history | 8 turns, 24 KiB; 4 KiB per stored reply | Drop oldest turns to fit bounded provider packet |
| Local Whisper | Model file at most 256 MiB; 4 CPU threads | Load model once per voice worker; reuse native state |
| Wake audio | 4-second rolling wake window; 15-second command cap at mono 16 kHz | Energy gate avoids decoding silence; endpoint or cap finishes command |
| Voice handoff | One pending command | Backpressure; mute and suppression invalidate pending/late results |
| Speech request | 8 KiB text; ElevenLabs audio at most 8 MiB | Bounded response validation before playback |

SQLite uses WAL mode, a requested 2 MiB page cache, a 32,768-page main-database cap, automatic checkpoint threshold of 128 pages, and a 4 MiB retained-journal target. With 4 KiB database pages the main-file cap is approximately 128 MiB. Page size, indexes, other connections, transient WAL growth, TLS buffers and allocator overhead affect actual memory/storage use; the journal target is not a hard instantaneous WAL cap.

The library permits explicitly configured inventory/history/ledger limits within ceilings of 10,000/100,000/1,024 respectively. The CLI uses defaults; `config.example.toml` is not loaded yet. Tombstones preserve identity/version and consume inventory capacity rather than allowing unlimited delete/recreate churn.

## Ownership and persistence

- The collector owns bounded fingerprints and directory capabilities, not cached copies of every file. SQLite holds current resource text and its FTS index.
- Only changed content creates a new version. Duplicate observations refresh metadata without duplicate history entries.
- History records resource ID, version, kind, actor and timestamp. Previous full content versions are not an application-level archive.
- Separate page and tab records prevent a metadata-only browser update from replacing full page text. Multi-record observations use one atomic batch.
- Attribution is `Unknown` or `AgentCandidate`, never proof of human intent. Candidate matching is one-use and invalidated by intervening target changes.
- Provider outputs are plain text/typed routes. No generated tool execution or unlimited worker spawning is connected.
- The Native Messaging host is a bounded observation process, not a general command server.

## Freshness and privacy limits

File change notifications identify dirty candidates; scans reconcile them. Metadata shortcuts cannot prove byte identity against an adversarial filesystem that preserves all metadata, so this is practical freshness rather than cryptographic proof that no change occurred. Full content hashing happens when files are actually read.

Deletion tombstones remove content from current-resource lookup and the live FTS index. They are not a secure erasure guarantee for SQLite pages, WAL files or filesystem backups. Time-based retention, a complete history-delete UI, OS focus history, and semantic activity summaries are still planned. Proposed 24-hour raw/7-day derived retention in architecture documents is not implemented merely because it appears in example configuration.

Observation stays local until a typed or wake-initiated question sends selected evidence to Crusoe and request/routing state to Jev. Local draft generation also transmits its selected evidence. Optional ElevenLabs synthesis sends reply text to that provider. Hidden paths, private browser windows, excluded DOM inputs and non-approved origins are collection boundaries, not a claim that every remaining visible string is non-sensitive.

The wake worker inspects ambient decoded text transiently and publishes only commands following the wake word. It retains no ambient transcript log. Muting and owned speech suppression clear pending commands and discard late decode results. Native Whisper decoding cannot currently be interrupted mid-call; its bounded call finishes before the worker releases its model, while the UI remains independent.

## What to measure next

For the demo, measure idle and active RSS, store/FTS/WAL size, reads per unchanged scan, queue-overflow count, and end-of-request to verified response. Record the actual hardware and sample count. Do not claim a specific memory footprint or latency improvement until measured. Prioritize a working awareness-to-answer session over expanding scope to OCR, embeddings, continuous model summaries or arbitrary desktop control.
