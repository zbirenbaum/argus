# Filesystem Snapshots & Restore

## Merkle Tree

Three object types in CAS: blob (file content), tree (directory listing), commit (root tree + timestamp + parent).

- On every mutating event: update in-memory tree, rehash only affected subtree, create commit
- Each mutating event records tree_hash (Merkle root) — every event is a restore point
- Tree and commit objects streamed to S3 alongside content

## Checkpoints

- Full tree state serialized to binary every N events (default: 1000)
- Uploaded to S3: `s3://bucket/checkpoints/{agent_id}/{seq}.bin`
- Deserializable on request: `argus dump-checkpoint --seq N --format json`
- On restart: load latest checkpoint from S3, replay events after checkpoint seq

## Full Restore

Given timestamp T:
1. Binary search event log for largest seq where ts ≤ T (local first, S3 fallback)
2. Find nearest checkpoint before that seq (from S3 if not local)
3. Load checkpoint tree
4. Replay events from checkpoint to target seq
5. Walk final tree, pull content from CAS (local or S3) to target directory

## Single File Restore

Given (path, timestamp T):
1. Find seq at timestamp T
2. Scan event log backward for last event touching path
3. Read content from CAS by hash

## Diff Between Points

Given (timestamp A, timestamp B):
1. Compute tree at A and tree at B
2. Diff Merkle trees (only walk subtrees with different hashes)
3. Report: added, deleted, modified files with content diffs from CAS

## Restore Modes

**New directory (non-destructive):**
```
argus restore --timestamp <T> --target /data/restore/snapshot-1/
```
Original workspace untouched. Safe for inspection, diffing, forking.

**In-place (undo):**
```
argus restore --timestamp <T> --in-place --force
```
Pauses all traced processes (see `06-agent-controls.md`). Overwrites workspace to match target state. Creates pre-restore snapshot automatically. Resumes processes. Warning: processes may crash if in-memory state assumes changed files.

**Selective:**
```
argus restore --timestamp <T> --path /workspace/config.yaml
argus restore --timestamp <T> --path /workspace/output/ --in-place
```
Uses Merkle tree to identify changed files in subtree. Only writes files that differ.

**Undo last N:**
```
argus undo --last 5
argus undo --last-by-pid 42
```
Finds timestamp before Nth-most-recent mutating event. Per-process undo only viable if writes don't overlap with other processes.
